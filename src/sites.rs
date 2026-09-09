//! Site identification over the network — `engine/sites.py`.
//!
//! Two lookups, both for a coordinate the local `[sites]` database does not
//! already know: **reverse geocoding** (Nominatim/OpenStreetMap) for the
//! site's postal address, and **sky quality** (lightpollutionmap.info's World
//! Atlas 2015 layer) for its artificial brightness, converted to SQM and then
//! to a Bortle class. The result is written back into `config.ini` so a site
//! is looked up once, ever.
//!
//! **Everything here is optional and fails soft.** No `[secret]` section, no
//! API key, no e-mail address, no network, a refused request, a malformed
//! response — every one of those falls back to the `[defaults]` values and
//! the run continues. An offline run and a run with no `[secret]` produce
//! byte-identical output to the wholly-offline v2.1.3 behaviour, which is
//! what keeps the golden corpus (whose `golden_config.ini` has no `[secret]`)
//! valid with no network access at all.
//!
//! ## What had to be reimplemented rather than copied
//!
//! `get_bortle_sqm` uses `requests` directly in Python source, so its request
//! is copied line for line. `reverse_geocode` does not: it calls **geopy**, a
//! library, and there is no Python source to copy. The request was therefore
//! reconstructed from *geopy 2.4.1's own source* rather than guessed —
//! `Nominatim.reverse` and `Geocoder._coerce_point_to_string` — which is the
//! same copy-don't-reimplement discipline applied one layer down.
//!
//! Three details from that reading are load-bearing, and each is pinned by a
//! test below:
//!
//! 1. **The two endpoints format coordinates differently.** Nominatim gets
//!    geopy's `_format_coordinate` rule (see [`format_coordinate`]);
//!    lightpollutionmap gets plain `str(float)` from an f-string.
//! 2. **lightpollutionmap's `qd` parameter is `lon,lat`** — reversed relative
//!    to Nominatim's separate `lat=`/`lon=`. Two adjacent call sites with
//!    opposite conventions; transposing them yields a real Bortle number for
//!    the wrong place, which no error would reveal.
//! 3. **Nothing throttles these requests.** Python wraps geopy in no
//!    `RateLimiter`, so a session with several unknown site clusters fires
//!    several requests back to back. Reproduced rather than "fixed":
//!    diverging here would change behaviour the parity contract covers.
//!    (Nominatim's usage policy asks for 1 req/sec, and blocks by
//!    User-Agent — which is built from the user's own `EMAIL_ADDRESS`.)

use crate::numeric::{python_repr_f64, python_round};

/// The World Atlas 2015 dataset, and the constants of the standard
/// conversion from artificial brightness to mag/arcsec². Both carried over
/// verbatim from v1.4.x via v2.2.x: changing either silently reclassifies
/// every site.
const WORLD_ATLAS_LAYER: &str = "wa_2015";
const BRIGHTNESS_OFFSET: f64 = 0.171168465;
const BRIGHTNESS_SCALER: f64 = 108_000_000.0;

/// lightpollutionmap.info issues 16-character alphanumeric keys.
const API_KEY_LENGTH: usize = 16;

/// Nominatim's terms of service require a contact address in the user agent.
/// Without one no request is made, rather than an anonymous one.
const USER_AGENT_PREFIX: &str = "AstroBinUpload.py_";

/// Nominatim's reverse endpoint, as geopy 2.4.1 builds it
/// (`_DEFAULT_NOMINATIM_DOMAIN` + `reverse_path`, over https).
const NOMINATIM_REVERSE_URL: &str = "https://nominatim.openstreetmap.org/reverse";

/// `sqm_to_bortle` — SQM to Bortle class.
///
/// The nine bands are those used since v1.4.x. They are asymmetric and the
/// boundaries are inclusive as written; reproduced exactly rather than tidied
/// into a table, because every site already recorded in a user's `[sites]`
/// section was classified by this function.
///
/// Returns 9 — the safest over-estimate of light pollution — for a value that
/// is not a number, matching the v1.4.x fallback. In Rust that can only be
/// NaN, since the type system rules out Python's `str`/`bool` cases.
pub fn sqm_to_bortle(sqm: f64) -> i64 {
    crate::log_info!("sqm_to_bortle", 76, "Converting SQM value {} to Bortle scale", python_repr_f64(sqm));
    if sqm.is_nan() {
        crate::log_error!(
            "sqm_to_bortle",
            100,
            "Could not convert SQM to Bortle (SQM must be a number); assuming Bortle 9"
        );
        return 9;
    }
    if sqm > 21.99 {
        1
    } else if (21.50..=21.99).contains(&sqm) {
        2
    } else if (21.25..=21.49).contains(&sqm) {
        3
    } else if (20.50..=21.24).contains(&sqm) {
        4
    } else if (19.50..=20.49).contains(&sqm) {
        5
    } else if (18.50..=19.49).contains(&sqm) {
        6
    } else if (17.50..=18.49).contains(&sqm) {
        7
    } else if (17.00..=17.49).contains(&sqm) {
        8
    } else {
        9
    }
}

/// `brightness_to_sqm` — the World Atlas conversion, to two decimals.
pub fn brightness_to_sqm(artificial_brightness: f64) -> f64 {
    python_round(
        ((artificial_brightness + BRIGHTNESS_OFFSET) / BRIGHTNESS_SCALER).log10() / -0.4,
        2,
    )
}

/// `is_valid_api_key` — 16 characters, alphanumeric.
///
/// Python's `str.isalnum()` is Unicode-aware and so is `char::is_alphanumeric`,
/// so the two agree beyond ASCII as well.
pub fn is_valid_api_key(api_key: &str) -> bool {
    api_key.chars().count() == API_KEY_LENGTH && !api_key.is_empty() && api_key.chars().all(char::is_alphanumeric)
}

/// `is_valid_api_endpoint` — any non-blank string.
pub fn is_valid_api_endpoint(api_endpoint: &str) -> bool {
    !api_endpoint.trim().is_empty()
}

/// `redact_api_key` — removes the key from a message before it is logged or
/// returned (upstream v2.2.1).
///
/// The key rides in the request URL's query string, and an HTTP error message
/// carries that URL, so without this an ordinary 404 or timeout writes the
/// user's credential into `AstroBinUploader.log` — the file most likely to be
/// attached to a bug report. The key is validated as alphanumeric, so it is
/// never percent-encoded in a URL and a literal replacement is exact.
pub fn redact_api_key(text: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        return text.to_string();
    }
    text.replace(api_key, "<redacted>")
}

/// geopy's `_format_coordinate` (`geocoders/base.py`), which decides the exact
/// `lat=`/`lon=` strings in a Nominatim request:
///
/// ```text
/// if abs(coordinate) >= 1: return coordinate   # -> str(float), via %s
/// return f"{coordinate:.7f}"                   # fixed 7 decimal places
/// ```
///
/// Not cosmetic. A longitude like `-0.1231` takes the second branch and is
/// sent as `-0.1231000`, while a latitude of `52.2484` takes the first and is
/// sent unchanged — so a naive `to_string()` would differ from Python on
/// every request from a site near the prime meridian or the equator.
pub fn format_coordinate(coordinate: f64) -> String {
    if coordinate.abs() >= 1.0 {
        python_repr_f64(coordinate)
    } else {
        format!("{coordinate:.7}")
    }
}

/// The outcome of one sky-quality request: Bortle, SQM, an error message
/// (`None` on success), and whether the key and endpoint looked valid.
/// `(0, 0.0)` in the first two means "use the defaults", exactly as the
/// Python tuple does.
pub struct SkyQuality {
    pub bortle: i64,
    pub sqm: f64,
    pub error: Option<String>,
    /// `#[allow(dead_code)]`: the last two members of Python's return tuple.
    /// Nothing reads them there either — `resolve` decides on `(0, 0)` alone
    /// — but they are part of the function's contract and the tests assert
    /// them, so they are carried rather than quietly dropped.
    #[allow(dead_code)]
    pub key_ok: bool,
    #[allow(dead_code)]
    pub endpoint_ok: bool,
}

/// One HTTP GET, behind a trait so the request path is testable without a
/// network — the same shape as the injected fake `requests` module in
/// upstream's `tests/test_sites.py`.
pub trait HttpTransport {
    /// Performs the GET. `Ok(body)` for a 2xx; `Err(message)` otherwise, with
    /// the message shaped like the exception text Python would have logged.
    fn get(&self, url: &str, params: &[(String, String)], user_agent: Option<&str>) -> Result<String, String>;
}

/// The real transport: `ureq` over rustls with bundled roots, so there is no
/// system OpenSSL to depend on and the single-binary premise survives.
pub struct UreqTransport;

impl HttpTransport for UreqTransport {
    fn get(&self, url: &str, params: &[(String, String)], user_agent: Option<&str>) -> Result<String, String> {
        let mut req = ureq::get(url);
        for (k, v) in params {
            req = req.query(k, v);
        }
        if let Some(ua) = user_agent {
            req = req.header("User-Agent", ua);
        }
        // `requests`' raise_for_status message is reproduced for HTTP status
        // errors, which is the reproducible half: `{code} {Client,Server}
        // Error: {reason} for url: {url}`. Transport-level failures (DNS,
        // timeout, TLS) carry urllib3 internals in Python and cannot be
        // matched; those messages differ and that difference is documented
        // rather than faked.
        match req.call() {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let body = resp
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| format!("could not read response body: {e}"))?;
                if (200..300).contains(&status) {
                    Ok(body)
                } else {
                    Err(http_status_message(status, resp.status().canonical_reason(), url))
                }
            }
            Err(ureq::Error::StatusCode(code)) => {
                let reason = ureq::http::StatusCode::from_u16(code)
                    .ok()
                    .and_then(|s| s.canonical_reason())
                    .unwrap_or("Unknown");
                Err(http_status_message(code, Some(reason), url))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// `requests`' `raise_for_status` message, which is what Python logs.
fn http_status_message(status: u16, reason: Option<&str>, url: &str) -> String {
    let kind = if (400..500).contains(&status) {
        "Client Error"
    } else if (500..600).contains(&status) {
        "Server Error"
    } else {
        "Error"
    };
    format!(
        "{status} {kind}: {} for url: {url}",
        reason.unwrap_or("Unknown")
    )
}

/// `get_bortle_sqm` — Bortle and SQM for one coordinate.
pub fn get_bortle_sqm(
    transport: &dyn HttpTransport,
    lat: f64,
    lon: f64,
    api_key: &str,
    api_endpoint: &str,
) -> SkyQuality {
    crate::log_info!("get_bortle_sqm", 178, "");
    crate::log_info!("get_bortle_sqm", 179, "GETTING BORTLE SCALE AND SQM VALUE");
    crate::log_info!(
        "get_bortle_sqm",
        180,
        "Retrieving Bortle scale and SQM for coordinates ({}, {})",
        python_repr_f64(lat),
        python_repr_f64(lon)
    );

    let key_ok = is_valid_api_key(api_key);
    let endpoint_ok = is_valid_api_endpoint(api_endpoint);

    if !key_ok || !endpoint_ok {
        crate::log_error!("get_bortle_sqm", 190, "API Key or Endpoint is invalid/missing.");
        return SkyQuality {
            bortle: 0,
            sqm: 0.0,
            error: Some("Invalid credentials".to_string()),
            key_ok,
            endpoint_ok,
        };
    }

    // `qd` is `lon,lat` -- reversed relative to Nominatim's lat/lon, and
    // formatted with a plain f-string (`str(float)`), not geopy's rule.
    let params = vec![
        ("ql".to_string(), WORLD_ATLAS_LAYER.to_string()),
        ("qt".to_string(), "point".to_string()),
        (
            "qd".to_string(),
            format!(
                "{},{}",
                python_repr_f64(lon),
                python_repr_f64(lat)
            ),
        ),
        ("key".to_string(), api_key.to_string()),
    ];

    crate::log_info!("get_bortle_sqm", 204, "Sending request to API endpoint");
    let body = match transport.get(api_endpoint, &params, None) {
        Ok(body) => body,
        Err(e) => {
            // Redacted before it is logged *or* returned: the returned string
            // is logged again by `SiteLookup::resolve` as "API error: ...",
            // so both routes out of here would otherwise carry the key.
            let message = redact_api_key(&e, api_key);
            crate::log_error!("get_bortle_sqm", 234, "Sky quality lookup failed: {message}");
            return SkyQuality {
                bortle: 0,
                sqm: 0.0,
                error: Some(format!("Request Error: {message}")),
                key_ok,
                endpoint_ok,
            };
        }
    };

    if body.trim() == "Invalid authentication." {
        crate::log_error!(
            "get_bortle_sqm",
            209,
            "Authentication error: Missing or invalid API key"
        );
        return SkyQuality {
            bortle: 0,
            sqm: 0.0,
            error: Some("Authentication error".to_string()),
            key_ok: false,
            endpoint_ok,
        };
    }

    // `float(response.text)`: Python's builtin, which is correctly rounded --
    // and so is Rust's `str::parse::<f64>`, so the two agree on every decimal
    // body this endpoint returns. A failure here is an ordinary degradation,
    // not a crash.
    let brightness = match body.trim().parse::<f64>() {
        Ok(b) => b,
        Err(_) => {
            let message = redact_api_key(
                &format!("could not convert string to float: {:?}", body.trim()),
                api_key,
            );
            crate::log_error!("get_bortle_sqm", 234, "Sky quality lookup failed: {message}");
            return SkyQuality {
                bortle: 0,
                sqm: 0.0,
                error: Some(format!("Request Error: {message}")),
                key_ok,
                endpoint_ok,
            };
        }
    };

    let sqm = brightness_to_sqm(brightness);
    let bortle = sqm_to_bortle(sqm);
    crate::log_info!(
        "get_bortle_sqm",
        215,
        "Retrieved Bortle scale: {bortle}, SQM value: {}",
        python_repr_f64(sqm)
    );
    SkyQuality {
        bortle,
        sqm,
        error: None,
        key_ok,
        endpoint_ok,
    }
}

/// `reverse_geocode` — coordinate to postal address via Nominatim.
///
/// Returns `None` when the lookup was skipped or failed for any reason; the
/// caller then uses the `[defaults]` site.
pub fn reverse_geocode(
    transport: &dyn HttpTransport,
    lat: f64,
    lon: f64,
    email: &str,
) -> Option<String> {
    if email.trim().is_empty() {
        crate::log_warning!(
            "reverse_geocode",
            258,
            "No EMAIL_ADDRESS in [secret]; skipping reverse geocoding \
             (Nominatim's terms of service require a contact address)."
        );
        return None;
    }

    // geopy 2.4.1's `Nominatim.reverse`: format=json and addressdetails=1 are
    // always sent; `zoom` is omitted when None (the default), and
    // `namedetails` when False (also the default).
    let params = vec![
        ("lat".to_string(), format_coordinate(lat)),
        ("lon".to_string(), format_coordinate(lon)),
        ("format".to_string(), "json".to_string()),
        ("addressdetails".to_string(), "1".to_string()),
    ];
    let user_agent = format!("{USER_AGENT_PREFIX}{email}");

    let body = match transport.get(NOMINATIM_REVERSE_URL, &params, Some(&user_agent)) {
        Ok(body) => body,
        Err(e) => {
            crate::log_warning!(
                "reverse_geocode",
                281,
                "Geocoding error: {e}. Using default site information"
            );
            return None;
        }
    };

    // geopy's `_parse_json`: an `error` member means no result (it maps
    // "Unable to geocode" to None), and `location.address` is `display_name`.
    match parse_display_name(&body) {
        Some(address) => {
            crate::log_info!("reverse_geocode", 272, "Using location string: {address}");
            Some(address)
        }
        None => {
            crate::log_warning!(
                "reverse_geocode",
                270,
                "No address found for ({}, {})",
                python_repr_f64(lat),
                python_repr_f64(lon)
            );
            None
        }
    }
}

/// Pulls `display_name` out of Nominatim's JSON.
///
/// Deliberately a small hand-written scan rather than a JSON dependency: the
/// response shape needed here is one string member of a flat object, and the
/// project's premise is a binary with nothing to install. Returns `None` for
/// an `error` member, matching geopy's own handling.
fn parse_display_name(body: &str) -> Option<String> {
    if json_string_member(body, "error").is_some() {
        return None;
    }
    json_string_member(body, "display_name")
}

/// Reads one top-level string member from a JSON object, decoding the escapes
/// that occur in an address (`\"`, `\\`, `\/`, `\uXXXX` — Nominatim escapes
/// non-ASCII when it escapes at all).
fn json_string_member(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut rest = body;
    loop {
        let at = rest.find(&needle)?;
        let after = &rest[at + needle.len()..];
        let after = after.trim_start();
        let Some(after) = after.strip_prefix(':') else {
            rest = &rest[at + needle.len()..];
            continue;
        };
        let after = after.trim_start();
        let Some(after) = after.strip_prefix('"') else {
            // Non-string value (an object, a number): not what we want.
            rest = &rest[at + needle.len()..];
            continue;
        };
        return Some(decode_json_string(after));
    }
}

/// Decodes a JSON string body up to its closing quote.
fn decode_json_string(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(decoded) => out.push(decoded),
                        None => {
                            out.push_str("\\u");
                            out.push_str(&hex);
                        }
                    }
                }
                Some(other) => out.push(other), // covers \" \\ \/
                None => break,
            },
            other => out.push(other),
        }
    }
    out
}

/// What one resolved coordinate produced: the site's name, and the sky
/// quality to record with it. `SiteLookup::resolve`'s `{'site', 'bortle',
/// 'sqm'}` dict.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub site: String,
    pub bortle: i64,
    pub sqm: f64,
}

/// `SiteLookup` — holds the `[secret]` credentials and answers one question
/// per cluster: *what is this place, and how dark is it?*
///
/// When it cannot answer — offline, no credentials, a failed request — it
/// says so and the caller uses the `[defaults]` values, which is precisely
/// what the whole pipeline did between v2.0.0 and v2.1.3.
pub struct SiteLookup<'a> {
    transport: &'a dyn HttpTransport,
    email: Option<String>,
    api_key: Option<String>,
    api_endpoint: Option<String>,
    default_site: String,
    default_bortle: i64,
    default_sqm: f64,
    /// The raw `[defaults]` strings for BORTLE/SQM, as configobj would hand
    /// them back -- *not* `default_bortle`/`default_sqm` stringified.
    /// `_normalize_defaults` never coerces types (`{k.upper()...: v for ...}`,
    /// `v` untouched), so `resolve`'s "Sky quality unavailable" line
    /// interpolates whatever the user wrote: `SQM = 21` logs `SQM 21`, not
    /// `SQM 21.0`. Measured against live Python (2026-09-09) rather than
    /// assumed from the coerced value used everywhere else -- the two only
    /// diverge for this one log line, since `resolve`'s *returned* dict does
    /// coerce (`int(float(bortle))`, `float(sqm)`) before this struct's
    /// other fields are read.
    default_bortle_raw: String,
    default_sqm_raw: String,
    /// False when no usable `[secret]` was supplied, in which case `resolve`
    /// is never called and no request is ever made.
    pub enabled: bool,
}

impl<'a> SiteLookup<'a> {
    /// `SiteLookup.__init__`.
    ///
    /// The section is written as `KEY = ENDPOINT` pairs plus `EMAIL_ADDRESS`,
    /// so **the API key is a *key* of the section, not a value** — the shape
    /// v1.4.x's `config.ini.example` documents and users already have on
    /// disk. That is why `[secret]` is carried into `AppConfig` with its key
    /// case and spelling untouched (Phase 7D).
    pub fn new(
        transport: &'a dyn HttpTransport,
        cfg: &crate::appconfig::AppConfig,
    ) -> Self {
        let mut email = None;
        let mut api_key = None;
        let mut api_endpoint = None;

        for (k, v) in &cfg.secret {
            let key = k.trim();
            if key.to_ascii_uppercase() == "EMAIL_ADDRESS" {
                email = Some(v.as_str().trim().to_string());
            } else if is_valid_api_key(key) {
                api_key = Some(key.to_string());
                api_endpoint = Some(v.as_str().trim().to_string());
            }
        }

        let has_email = email.as_deref().is_some_and(|e| !e.is_empty());
        let enabled = has_email || api_key.is_some();
        if !enabled && !cfg.secret.is_empty() {
            crate::log_warning!(
                "__init__",
                325,
                "[secret] is present but carries neither a usable 16-character \
                 API key nor an EMAIL_ADDRESS; no network lookups will be made."
            );
        }

        Self {
            transport,
            email,
            api_key,
            api_endpoint,
            default_site: default_str(cfg, "SITE", "Unknown Site"),
            default_bortle: default_int(cfg, "BORTLE", 4),
            default_sqm: default_float(cfg, "SQM", 21.0),
            // Python's fallback is the literal `4` / `21.0`, which an
            // f-string renders as "4" / "21.0" -- matched here, not "21".
            default_bortle_raw: default_str(cfg, "BORTLE", "4"),
            default_sqm_raw: default_str(cfg, "SQM", "21.0"),
            enabled,
        }
    }

    /// `SiteLookup.resolve` — looks up one coordinate.
    ///
    /// `None` means nothing at all could be resolved, which tells the caller
    /// to use the defaults untouched and to write nothing back.
    pub fn resolve(&self, lat: f64, lon: f64) -> Option<Resolved> {
        if !self.enabled {
            return None;
        }

        crate::log_info!("resolve", 347, "");
        crate::log_info!("resolve", 348, "PROCESSING NEW LOCATION");
        crate::log_info!(
            "resolve",
            349,
            "Site location does not exist in existing sites: ({}, {})",
            python_repr_f64(lat),
            python_repr_f64(lon)
        );

        let site = match self.email.as_deref() {
            Some(email) if !email.is_empty() => {
                reverse_geocode(self.transport, lat, lon, email)
            }
            // `if self.email else None` -- no request without a contact
            // address, and no warning either: the constructor already said so.
            _ => None,
        };

        let mut bortle: Option<i64> = None;
        let mut sqm: Option<f64> = None;
        if let (Some(key), Some(endpoint)) = (self.api_key.as_deref(), self.api_endpoint.as_deref())
        {
            let r = get_bortle_sqm(self.transport, lat, lon, key, endpoint);
            if let Some(error) = &r.error {
                crate::log_warning!("resolve", 360, "API error: {error}");
            }
            // `if not (b == 0 and s == 0)`: the sentinel means "use defaults",
            // and any other pair -- including a genuine 0 on one side -- is a
            // real answer.
            if !(r.bortle == 0 && r.sqm == 0.0) {
                bortle = Some(r.bortle);
                sqm = Some(r.sqm);
            }
        }

        if site.is_none() && bortle.is_none() {
            crate::log_warning!(
                "resolve",
                365,
                "Neither the site name nor its sky quality could be resolved; \
                 falling back to the [defaults] values."
            );
            return None;
        }

        let (bortle, sqm) = match (bortle, sqm) {
            (Some(b), Some(s)) => (b, s),
            _ => {
                crate::log_warning!(
                    "resolve",
                    374,
                    "Sky quality unavailable, using defaults: Bortle {}, SQM {}",
                    self.default_bortle_raw,
                    self.default_sqm_raw
                );
                (self.default_bortle, self.default_sqm)
            }
        };
        let site = site.unwrap_or_else(|| self.default_site.clone());

        crate::log_info!(
            "resolve",
            380,
            "Processed new location: {site}, Bortle: {bortle}, SQM: {}",
            python_repr_f64(sqm)
        );
        Some(Resolved { site, bortle, sqm })
    }

    /// `SiteLookup.save` — writes a resolved site into `[sites]` so it is
    /// never looked up twice.
    ///
    /// The one operation that touches a file the user owns, so it is the most
    /// conservative: it refuses to write a site it could not name, never
    /// rewrites an entry that already exists, and a failed write is logged
    /// and swallowed rather than losing the run's real output.
    pub fn save(
        &self,
        site: &str,
        lat: f64,
        lon: f64,
        bortle: i64,
        sqm: f64,
        config_path: &std::path::Path,
    ) -> bool {
        // Python's first guard: writing the default site under its own name
        // would poison the database with a bogus entry.
        if site.is_empty() || site == self.default_site {
            return false;
        }
        let resolved = crate::config_write::ResolvedSite {
            name: site,
            latitude: lat,
            longitude: lon,
            bortle,
            sqm,
        };
        match crate::config_write::save_site(config_path, &resolved) {
            Ok(true) => {
                crate::log_info!(
                    "save",
                    430,
                    "Saved new site to {}: {site}",
                    config_path.display()
                );
                println!(
                    "New observing site recorded in {}: {site}",
                    config_path.display()
                );
                true
            }
            // `if site in cfg[section_name]: return False` -- already present,
            // nothing written and nothing said.
            Ok(false) => false,
            Err(e) => {
                crate::log_error!(
                    "save",
                    434,
                    "Could not save the new site to {}: {e}",
                    config_path.display()
                );
                false
            }
        }
    }
}

/// `config.defaults.get(KEY, fallback)` as a string.
fn default_str(cfg: &crate::appconfig::AppConfig, key: &str, fallback: &str) -> String {
    cfg.defaults
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| fallback.to_string())
}

/// `int(float(...))` on a `[defaults]` value, falling back rather than
/// failing: this is the degradation path, and it must not raise.
fn default_int(cfg: &crate::appconfig::AppConfig, key: &str, fallback: i64) -> i64 {
    cfg.defaults
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str().trim().parse::<f64>().ok())
        .map(|f| f as i64)
        .unwrap_or(fallback)
}

fn default_float(cfg: &crate::appconfig::AppConfig, key: &str, fallback: f64) -> f64 {
    cfg.defaults
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str().trim().parse::<f64>().ok())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every band boundary, both sides, against the Python function's own
    /// values. These classify every site a user has ever recorded, so a
    /// shifted boundary silently reclassifies their history.
    #[test]
    fn bortle_bands_including_their_boundaries() {
        for (sqm, expected) in [
            (22.00, 1),
            (21.995, 1),
            (21.99, 2),
            (21.50, 2),
            (21.49, 3),
            (21.25, 3),
            (21.24, 4),
            (20.50, 4),
            (20.49, 5),
            (19.50, 5),
            (19.49, 6),
            (18.50, 6),
            (18.49, 7),
            (17.50, 7),
            (17.49, 8),
            (17.00, 8),
            (16.99, 9),
            (0.0, 9),
        ] {
            assert_eq!(sqm_to_bortle(sqm), expected, "sqm {sqm}");
        }
    }

    #[test]
    fn a_non_numeric_sqm_assumes_the_worst_sky() {
        assert_eq!(sqm_to_bortle(f64::NAN), 9);
    }

    /// Values read off the live Python function on 2026-09-09, not computed
    /// here and not recalled: the first draft of this test guessed three of
    /// them and two were wrong, which is the whole argument for measuring.
    #[test]
    fn world_atlas_brightness_converts_to_sqm() {
        assert_eq!(brightness_to_sqm(0.1), 21.5);
        assert_eq!(brightness_to_sqm(0.0), 22.0);
        assert_eq!(brightness_to_sqm(1.0), 19.91);
        assert_eq!(brightness_to_sqm(10.0), 17.57);
        assert_eq!(brightness_to_sqm(100.0), 15.08);
        assert_eq!(brightness_to_sqm(0.5), 20.52);
        // The offset itself: brightness + OFFSET is exactly 2*OFFSET here.
        assert_eq!(brightness_to_sqm(0.171168465), 21.25);
    }

    #[test]
    fn api_key_validation() {
        assert!(is_valid_api_key("ABCDEF1234567890"));
        assert!(!is_valid_api_key("too-short"));
        assert!(!is_valid_api_key(""));
        assert!(!is_valid_api_key("ABCDEF123456789012345"));
        assert!(!is_valid_api_key("ABCDEF-234567890")); // 16 chars, not alnum
    }

    #[test]
    fn api_endpoint_validation() {
        assert!(is_valid_api_endpoint("https://example.com/"));
        assert!(!is_valid_api_endpoint(""));
        assert!(!is_valid_api_endpoint("   "));
    }

    /// geopy's rule, measured against live geopy 2.4.1 on 2026-09-09. The
    /// `< 1` cases are the ones a naive `to_string()` gets wrong -- and the
    /// maintainer's own site longitude, -0.1231, is one of them.
    #[test]
    fn coordinates_are_formatted_the_way_geopy_formats_them() {
        assert_eq!(format_coordinate(52.2484), "52.2484");
        assert_eq!(format_coordinate(-111.8833), "-111.8833");
        assert_eq!(format_coordinate(40.75), "40.75");
        assert_eq!(format_coordinate(-0.1231), "-0.1231000");
        assert_eq!(format_coordinate(0.0), "0.0000000");
        assert_eq!(format_coordinate(52.24839999), "52.24839999");
        assert_eq!(format_coordinate(-0.12310001), "-0.1231000");
    }

    #[test]
    fn redaction_removes_the_key_and_leaves_everything_else() {
        let msg = "404 Client Error: Not Found for url: https://x/?key=ABCDEF1234567890";
        let out = redact_api_key(msg, "ABCDEF1234567890");
        assert!(!out.contains("ABCDEF1234567890"));
        assert!(out.ends_with("key=<redacted>"));
        assert_eq!(redact_api_key("no key here", "ABCDEF1234567890"), "no key here");
        assert_eq!(redact_api_key("some text", ""), "some text");
    }

    /// A transport that records what it was asked for and replays a canned
    /// answer — the Rust equivalent of upstream's injected fake `requests`.
    struct FakeTransport {
        response: Result<String, String>,
        seen: std::cell::RefCell<Vec<(String, Vec<(String, String)>, Option<String>)>>,
    }

    impl FakeTransport {
        fn ok(body: &str) -> Self {
            Self {
                response: Ok(body.to_string()),
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn err(message: &str) -> Self {
            Self {
                response: Err(message.to_string()),
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn last(&self) -> (String, Vec<(String, String)>, Option<String>) {
            self.seen.borrow().last().cloned().unwrap()
        }
    }

    impl HttpTransport for FakeTransport {
        fn get(
            &self,
            url: &str,
            params: &[(String, String)],
            user_agent: Option<&str>,
        ) -> Result<String, String> {
            self.seen.borrow_mut().push((
                url.to_string(),
                params.to_vec(),
                user_agent.map(str::to_string),
            ));
            self.response.clone()
        }
    }

    fn param(params: &[(String, String)], key: &str) -> String {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_successful_lookup_converts_and_classifies() {
        let fake = FakeTransport::ok("0.1");
        let r = get_bortle_sqm(&fake, 52.2484, -0.1231, "ABCDEF1234567890", "https://x/");
        assert_eq!((r.bortle, r.sqm), (2, 21.5));
        assert!(r.error.is_none());
        assert!(r.key_ok && r.endpoint_ok);
    }

    /// The transposition guard: `qd` is `lon,lat`, not `lat,lon`. Getting
    /// this backwards returns a real Bortle number for the wrong place, and
    /// nothing downstream would flag it.
    #[test]
    fn the_sky_quality_query_sends_lon_then_lat() {
        let fake = FakeTransport::ok("0.1");
        get_bortle_sqm(&fake, 52.2484, -0.1231, "ABCDEF1234567890", "https://x/");
        let (url, params, ua) = fake.last();
        assert_eq!(url, "https://x/");
        assert_eq!(param(&params, "qd"), "-0.1231,52.2484");
        assert_eq!(param(&params, "ql"), "wa_2015");
        assert_eq!(param(&params, "qt"), "point");
        assert_eq!(param(&params, "key"), "ABCDEF1234567890");
        assert!(ua.is_none(), "the sky-quality request sends no user agent");
    }

    #[test]
    fn invalid_credentials_short_circuit_before_any_request() {
        struct Exploding;
        impl HttpTransport for Exploding {
            fn get(&self, _: &str, _: &[(String, String)], _: Option<&str>) -> Result<String, String> {
                panic!("a request was made despite invalid credentials");
            }
        }
        let r = get_bortle_sqm(&Exploding, 52.0, -0.1, "too-short", "https://x/");
        assert_eq!((r.bortle, r.sqm), (0, 0.0));
        assert!(!r.key_ok);
        assert_eq!(r.error.as_deref(), Some("Invalid credentials"));
    }

    #[test]
    fn the_authentication_error_body_is_recognised() {
        let fake = FakeTransport::ok("Invalid authentication.");
        let r = get_bortle_sqm(&fake, 52.0, -0.1, "ABCDEF1234567890", "https://x/");
        assert_eq!((r.bortle, r.sqm), (0, 0.0));
        assert_eq!(r.error.as_deref(), Some("Authentication error"));
        assert!(!r.key_ok);
    }

    #[test]
    fn a_network_failure_degrades_rather_than_aborting_the_run() {
        let fake = FakeTransport::err("network unreachable");
        let r = get_bortle_sqm(&fake, 52.0, -0.1, "ABCDEF1234567890", "https://x/");
        assert_eq!((r.bortle, r.sqm), (0, 0.0));
        assert!(r.error.unwrap().contains("network unreachable"));
    }

    #[test]
    fn an_unparseable_body_degrades_too() {
        let fake = FakeTransport::ok("<html>error page</html>");
        let r = get_bortle_sqm(&fake, 52.0, -0.1, "ABCDEF1234567890", "https://x/");
        assert_eq!((r.bortle, r.sqm), (0, 0.0));
        assert!(r.error.is_some());
    }

    /// The port must preserve upstream v2.2.1's property that the key reaches
    /// neither the log nor the returned string, since `resolve` logs that
    /// string again.
    #[test]
    fn a_failed_request_never_carries_the_api_key_out() {
        let key = "ABCDEF1234567890";
        let fake = FakeTransport::err(&format!(
            "404 Client Error: Not Found for url: https://x/?ql=wa_2015&key={key}"
        ));
        let r = get_bortle_sqm(&fake, 52.0, -0.1, key, "https://x/");
        let error = r.error.unwrap();
        assert!(!error.contains(key), "the key escaped in: {error}");
        assert!(error.contains("<redacted>"));
    }

    #[test]
    fn reverse_geocoding_sends_geopys_request_shape() {
        let fake = FakeTransport::ok(r#"{"display_name": "Norton Close, Papworth Everard"}"#);
        let got = reverse_geocode(&fake, 52.2484, -0.1231, "someone@example.com");
        assert_eq!(got.as_deref(), Some("Norton Close, Papworth Everard"));

        let (url, params, ua) = fake.last();
        assert_eq!(url, "https://nominatim.openstreetmap.org/reverse");
        assert_eq!(param(&params, "lat"), "52.2484");
        assert_eq!(param(&params, "lon"), "-0.1231000");
        assert_eq!(param(&params, "format"), "json");
        assert_eq!(param(&params, "addressdetails"), "1");
        assert_eq!(ua.as_deref(), Some("AstroBinUpload.py_someone@example.com"));
        assert!(
            !params.iter().any(|(k, _)| k == "zoom" || k == "namedetails"),
            "geopy omits both at their defaults"
        );
    }

    #[test]
    fn no_email_means_no_request_at_all() {
        struct Exploding;
        impl HttpTransport for Exploding {
            fn get(&self, _: &str, _: &[(String, String)], _: Option<&str>) -> Result<String, String> {
                panic!("Nominatim was called without a contact address");
            }
        }
        assert!(reverse_geocode(&Exploding, 52.0, -0.1, "").is_none());
        assert!(reverse_geocode(&Exploding, 52.0, -0.1, "   ").is_none());
    }

    #[test]
    fn a_geocoding_failure_or_empty_result_degrades_to_none() {
        let failed = FakeTransport::err("timed out");
        assert!(reverse_geocode(&failed, 52.0, -0.1, "a@b.c").is_none());

        let no_result = FakeTransport::ok(r#"{"error": "Unable to geocode"}"#);
        assert!(reverse_geocode(&no_result, 52.0, -0.1, "a@b.c").is_none());
    }

    #[test]
    fn display_name_survives_json_escapes() {
        let body = r#"{"place_id":1,"display_name":"Café \"X\", 5\/7 Rue","lat":"52.2"}"#;
        assert_eq!(
            parse_display_name(body).as_deref(),
            Some("Café \"X\", 5/7 Rue")
        );
    }

    #[test]
    fn requests_status_messages_are_reproduced() {
        assert_eq!(
            http_status_message(404, Some("Not Found"), "https://x/"),
            "404 Client Error: Not Found for url: https://x/"
        );
        assert_eq!(
            http_status_message(503, Some("Service Unavailable"), "https://x/"),
            "503 Server Error: Service Unavailable for url: https://x/"
        );
    }

    /// A transport that answers Nominatim and lightpollutionmap differently
    /// by URL, so `SiteLookup::resolve` can be driven through the "geocoding
    /// succeeded, sky quality failed" branch specifically.
    struct SplitTransport {
        geocode_body: &'static str,
        sky_quality_result: Result<&'static str, &'static str>,
    }

    impl HttpTransport for SplitTransport {
        fn get(
            &self,
            url: &str,
            _params: &[(String, String)],
            _user_agent: Option<&str>,
        ) -> Result<String, String> {
            if url == NOMINATIM_REVERSE_URL {
                Ok(self.geocode_body.to_string())
            } else {
                self.sky_quality_result
                    .map(str::to_string)
                    .map_err(str::to_string)
            }
        }
    }

    fn app_config(text: &str) -> crate::appconfig::AppConfig {
        crate::appconfig::AppConfig::from_config(&crate::config::ConfigFile::parse_str(text).unwrap())
            .unwrap()
    }

    /// The bug this test exists for: `resolve`'s "Sky quality unavailable"
    /// line interpolates the *raw* `[defaults]` string (`SQM = 21` -> "SQM
    /// 21"), not the value coerced to `f64` and re-rendered ("SQM 21.0").
    /// Caught by running the equivalent live Python and comparing, not by
    /// reading the source a second time -- the first draft of `SiteLookup`
    /// got this wrong by reusing the coerced field.
    #[test]
    fn sky_quality_unavailable_logs_the_raw_defaults_string_not_a_reformatted_number() {
        let cfg = app_config(
            "[defaults]\nSITE = Home\nBORTLE = 4\nSQM = 21\n\
             [secret]\nYOUR_API_KEY = ABCDEF1234567890\nEMAIL_ADDRESS = a@b.c\n",
        );
        let transport = SplitTransport {
            geocode_body: r#"{"display_name": "Somewhere, Nowhere"}"#,
            sky_quality_result: Err("network unreachable"),
        };
        let lookup = SiteLookup::new(&transport, &cfg);
        assert!(lookup.enabled);
        let resolved = lookup.resolve(52.0, -0.1).unwrap();
        // The *returned* values are still coerced numbers (int(float(...)),
        // float(...)) -- only the log line differs. Confirmed via
        // default_sqm_raw directly, since the log itself isn't captured here.
        assert_eq!(resolved.site, "Somewhere, Nowhere");
        assert_eq!(resolved.bortle, 4);
        assert_eq!(resolved.sqm, 21.0);
        assert_eq!(lookup.default_sqm_raw, "21"); // not "21.0"
        assert_eq!(lookup.default_bortle_raw, "4");
    }

    #[test]
    fn a_resolved_site_can_be_saved_and_a_repeat_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!(
            "astrobin_sitelookup_save_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.ini");
        std::fs::write(&path, "[sites]\n").unwrap();

        let cfg = app_config("[defaults]\nSITE = Home\n");
        let transport = SplitTransport {
            geocode_body: "{}",
            sky_quality_result: Ok("0.1"),
        };
        let lookup = SiteLookup::new(&transport, &cfg);

        assert!(lookup.save("New Site", 1.0, 2.0, 4, 21.0, &path));
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[[New Site]]"));

        // Second save of the same site is a no-op -- file unchanged.
        assert!(!lookup.save("New Site", 9.0, 9.0, 9, 9.0, &path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), written);

        // The [defaults] SITE fallback is refused, even if asked for by name.
        assert!(!lookup.save("Home", 1.0, 2.0, 4, 21.0, &path));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
