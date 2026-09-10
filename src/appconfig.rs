//! `AppConfig` — the typed view of the INI file, mirroring `engine/loader.py`.
//!
//! `ConfigFile` is the parse; this is the normalisation the loader applies on
//! top of it, and the normalisation is not cosmetic:
//!
//! - `[defaults]` keys are upper-cased with spaces stripped (`sitelat` and
//!   `SiteLat` are one key). Note this is *not* the FITS-keyword mapping the
//!   Python docstring claims: `'Image Type'` normalises to `IMAGETYPE`, not
//!   `IMAGETYP`, so such a key injects a column nothing reads.
//! - `[override]` values become lists whether the INI wrote one or not.
//! - `[equipmentoverrides]` drops blank and `None` entries, so the generated
//!   template can list every overridable field without changing behaviour.
//! - `USEOBSDATE` is read off the *raw* `[defaults]` section, by that exact
//!   spelling, before key normalisation.
//!
//! Order is preserved throughout: `[defaults]` injection order decides the
//! column order of injected columns, and `[override]` order decides which
//! hardware key wins.

use anyhow::{bail, Result};

use crate::config::{ConfigFile, Section, Value};

#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    /// `[defaults]`, keys upper-cased and space-stripped, in file order.
    pub defaults: Vec<(String, Value)>,
    /// `[override]`: internal key -> candidate hardware keys, in file order.
    pub overrides: Vec<(String, Vec<String>)>,
    /// `[equipmentoverrides]`: forced literal values, sentinels removed.
    pub equipment_overrides: Vec<(String, String)>,
    /// `[filters]`: filter name -> AstroBin code, in file order.
    pub filters: Vec<(String, Value)>,
    /// `[sites]`: site name -> its subsection.
    pub sites: Vec<(String, Section)>,
    /// `[secret]`: the sky-quality API key/endpoint and contact e-mail --
    /// `YOUR_API_KEY = <endpoint>` plus `EMAIL_ADDRESS = <address>`, in
    /// file order, flat like `[filters]` (no key normalisation: `loader.py`
    /// only lower-cases *top-level* section names, never a section's own
    /// keys). Empty when the section is absent, which is how the network
    /// layer (Phase 7E) knows to stay offline -- exactly `models.py`'s
    /// `secret: Dict[str, Any] = field(default_factory=dict)`.
    ///
    /// `#[allow(dead_code)]`: parsed but not yet read by anything --
    /// `SiteLookup`'s port (Phase 7E) is the first consumer. Exercised in
    /// the meantime by this module's own tests and by `dump_parity`, which
    /// walks the raw `ConfigFile` (not `AppConfig`) and so already surfaces
    /// `[secret]` for `check_parity.py`.
    #[allow(dead_code)]
    pub secret: Vec<(String, Value)>,
    pub use_obs_date: bool,
    /// Decimal precision for coordinate rounding. `AppConfig.precision` in
    /// `models.py`; not configurable there either.
    pub precision: u32,
}

impl AppConfig {
    pub fn from_config(cfg: &ConfigFile) -> Result<Self> {
        let defaults_sec = cfg.section("defaults");

        // `str(defaults_sec.get('USEOBSDATE', 'True')).lower() == 'true'` --
        // read before key normalisation, so the spelling has to match exactly.
        let use_obs_date = defaults_sec
            .and_then(|s| s.get("USEOBSDATE"))
            .map(|v| v.as_str())
            .unwrap_or_else(|| "True".to_string())
            .to_ascii_lowercase()
            == "true";

        let defaults = defaults_sec
            .map(|s| {
                s.iter_ordered()
                    .map(|(k, v)| (normalize_key(k), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let overrides = cfg
            .section("override")
            .map(|s| {
                s.iter_ordered()
                    .map(|(k, v)| {
                        // Both shapes collapse to a list of trimmed strings;
                        // the key keeps its original spelling, because Stage 1
                        // writes the column under exactly that name.
                        let keys = v
                            .as_list()
                            .into_iter()
                            .map(|item| item.trim().to_string())
                            .collect();
                        // A6: an override onto a name that is not an
                        // internal column silently creates a phantom column
                        // nothing consumes, so the loader says so.
                        if !crate::constants::KNOWN_OVERRIDE_TARGETS
                            .contains(&k.to_lowercase().as_str())
                        {
                            crate::log_warning!(
                                "_normalize_overrides",
                                319,
                                "[override] target '{k}' does not match any recognized \
                                 internal column and will have no effect. Check for a \
                                 typo (did you mean one of: {}?)",
                                crate::constants::KNOWN_OVERRIDE_TARGETS.join(", ")
                            );
                        }
                        (k.clone(), keys)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut equipment_overrides = Vec::new();
        if let Some(sec) = cfg.section("equipmentoverrides") {
            for (k, v) in sec.iter_ordered() {
                let val = v.as_str();
                let val = val.trim();
                if val.is_empty() || val.eq_ignore_ascii_case("none") {
                    continue; // sentinel: "leave as found"
                }
                let key = normalize_key(k);
                if !crate::constants::KNOWN_OVERRIDE_TARGETS.contains(&key.to_lowercase().as_str())
                {
                    crate::log_warning!(
                        "_normalize_equipment_overrides",
                        344,
                        "[equipmentoverrides] target '{k}' does not match any recognized \
                         internal column and will have no effect."
                    );
                }
                equipment_overrides.push((key, val.to_string()));
            }
        }

        let filters = cfg
            .section("filters")
            .map(|s| s.iter_ordered().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let secret = cfg
            .section("secret")
            .map(|s| s.iter_ordered().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        // configobj preserves subsection order; `Section::sections` is a
        // BTreeMap, so recover file order from the parse instead of trusting
        // the map's ordering. Site lookup is by name, not position, so this is
        // only about making the dump stable.
        let sites = cfg
            .section("sites")
            .map(|s| {
                s.sections
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(AppConfig {
            defaults,
            overrides,
            equipment_overrides,
            filters,
            sites,
            secret,
            use_obs_date,
            precision: 4,
        })
    }

    /// `config.defaults.get(key, fallback)` as a float, the way
    /// `OpticalParameterStep` reads `HFR`.
    pub fn default_f64(&self, key: &str, fallback: f64) -> Result<f64> {
        match self.defaults.iter().find(|(k, _)| k == key) {
            None => Ok(fallback),
            Some((_, v)) => {
                let text = v.as_str();
                // `float(...)` on the config string: Python's correctly
                // rounded conversion, not the CSV tokenizer's.
                text.trim().parse::<f64>().map_err(|_| {
                    anyhow::anyhow!("[defaults] {key} = {text:?} is not a number")
                })
            }
        }
    }

    /// A `[defaults]` value as the scalar string that gets broadcast into a
    /// column. configobj turns a comma-separated value into a list, and
    /// assigning a list to a column raises in pandas unless its length happens
    /// to equal the row count — so refuse it here rather than invent a
    /// meaning.
    pub fn default_scalar(key: &str, value: &Value) -> Result<String> {
        match value {
            Value::Str(s) => Ok(s.clone()),
            Value::List(_) => bail!(
                "[defaults] {key} contains a comma, so configobj parsed it as a \
                 list; the Python side raises when broadcasting that into a \
                 column. Quote the value if the comma is part of it."
            ),
        }
    }
}

/// `k.upper().replace(' ', '')`.
fn normalize_key(k: &str) -> String {
    k.to_uppercase().replace(' ', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> AppConfig {
        AppConfig::from_config(&ConfigFile::parse_str(text).unwrap()).unwrap()
    }

    /// Note what the normalisation actually does with a spaced key:
    /// `'Image Type'.upper().replace(' ', '')` is `IMAGETYPE`, not the
    /// `IMAGETYP` FITS keyword `_normalize_defaults`' docstring claims it
    /// produces. The behaviour is copied, not the docstring.
    #[test]
    fn default_keys_are_upper_cased_and_despaced_in_file_order() {
        let c = cfg("[defaults]\nImage Type = LIGHT\nsitelat = 52.2484\n");
        assert_eq!(
            c.defaults
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            vec!["IMAGETYPE", "SITELAT"]
        );
    }

    #[test]
    fn use_obs_date_defaults_to_true_and_reads_the_raw_spelling() {
        assert!(cfg("[defaults]\nSITE = x\n").use_obs_date);
        assert!(!cfg("[defaults]\nUSEOBSDATE = False\n").use_obs_date);
        assert!(cfg("[defaults]\nUSEOBSDATE = true\n").use_obs_date);
    }

    #[test]
    fn overrides_are_always_lists_and_keep_their_key_spelling() {
        let c = cfg("[override]\nSQM = AOCSKYQ, AOCSKYQU\nSWCREATOR = CREATOR\n");
        assert_eq!(
            c.overrides,
            vec![
                ("SQM".to_string(), vec!["AOCSKYQ".into(), "AOCSKYQU".into()]),
                ("SWCREATOR".to_string(), vec!["CREATOR".into()]),
            ]
        );
    }

    #[test]
    fn equipment_override_sentinels_are_dropped() {
        let c = cfg("[equipmentoverrides]\nINSTRUME = None\nFOCNAME = ZWO EAF\nFWHEEL = \n");
        assert_eq!(
            c.equipment_overrides,
            vec![("FOCNAME".to_string(), "ZWO EAF".to_string())]
        );
    }

    #[test]
    fn a_comma_bearing_default_is_refused_rather_than_guessed() {
        let c = cfg("[defaults]\nSITE = Norton Close, Papworth\n");
        let (k, v) = &c.defaults[0];
        assert!(AppConfig::default_scalar(k, v).is_err());
    }

    #[test]
    fn hfr_default_is_read_as_a_float() {
        let c = cfg("[defaults]\nHFR = 1\n");
        assert_eq!(c.default_f64("HFR", 1.0).unwrap(), 1.0);
        assert_eq!(c.default_f64("MISSING", 1.0).unwrap(), 1.0);
    }

    /// `[secret]` is flat and unnormalised, like `[filters]`: the API key is
    /// a *key* of the section (loader.py's own comment in engine/sites.py),
    /// not a value, so key case and spelling must survive untouched for
    /// `SiteLookup`'s port to recognise it later.
    #[test]
    fn secret_is_parsed_flat_and_unnormalised_in_file_order() {
        let c = cfg(
            "[secret]\n\
             YOUR_API_KEY = https://www.lightpollutionmap.info/QueryRaster/\n\
             EMAIL_ADDRESS = astronomer@example.com\n",
        );
        assert_eq!(
            c.secret,
            vec![
                (
                    "YOUR_API_KEY".to_string(),
                    Value::Str("https://www.lightpollutionmap.info/QueryRaster/".to_string())
                ),
                (
                    "EMAIL_ADDRESS".to_string(),
                    Value::Str("astronomer@example.com".to_string())
                ),
            ]
        );
    }

    /// `models.py`'s `secret: Dict[str, Any] = field(default_factory=dict)`
    /// -- absent section, empty collection, not an error. This is how the
    /// network layer (Phase 7E) knows to stay fully offline.
    #[test]
    fn secret_is_empty_when_the_section_is_absent() {
        let c = cfg("[defaults]\nSITE = x\n");
        assert!(c.secret.is_empty());
    }
}
