//! Internal column names, mirroring `constants.py::InternalColumns`.
//!
//! Every raw header becomes one of these lower-case names in
//! `NormalizeHeadersStep` stage 2, and the rest of the pipeline reads only
//! these. Kept as constants for the same reason the Python side does: a typo
//! in a column name is silent — it creates or misses a column rather than
//! failing.

pub const IMAGE_TYPE: &str = "imagetyp";
pub const DURATION: &str = "exposure";
pub const BINNING: &str = "xbinning";
pub const SENSOR_COOLING: &str = "ccd-temp";
pub const MEAN_FWHM: &str = "fwhm";
pub const F_NUMBER: &str = "focratio";
pub const TEMPERATURE: &str = "foctemp";
pub const MEAN_SQM: &str = "sqm";
pub const FOCAL_LENGTH: &str = "focallen";
pub const PIXEL_SIZE: &str = "xpixsz";
pub const TARGET: &str = "object";
pub const SITE_LAT: &str = "sitelat";
pub const SITE_LONG: &str = "sitelong";
pub const BORTLE: &str = "bortle";
pub const FILENAME: &str = "filename";
pub const SOURCE_PATH: &str = "source_path";
/// The *raw* header key the extractor writes, before `NormalizeHeadersStep`
/// lower-cases it. The scan path never upper-cases column names, so the two
/// spellings coexist and mixing them up silently loses the column.
pub const SOURCE_PATH_RAW: &str = "SOURCE_PATH";
pub const NUMBER: &str = "number";
pub const DATE_OBS: &str = "date-obs";
pub const SITE_NAME: &str = "site";
pub const FILTER_NAME: &str = "filter";
pub const HFR: &str = "hfr";
pub const IMSCALE: &str = "imscale";
pub const GAIN: &str = "gain";
pub const EGAIN: &str = "egain";
/// Hybrid EGAIN/GAIN handshake key, built by `CalibrationMatcherStep`.
pub const GAIN_MATCH: &str = "gain_match";
pub const CAMERA: &str = "instrume";
pub const TELESCOPE: &str = "telescop";
pub const FOCUSER: &str = "focname";
pub const FILTER_WHEEL: &str = "fwheel";
pub const ROTATOR_NAME: &str = "rotname";
pub const SWCREATE: &str = "swcreate";

/// Normalised `IMAGETYP` values (`constants.py::ImageType`).
pub mod image_type {
    pub const LIGHT: &str = "LIGHT";
    pub const FLAT: &str = "FLAT";
    pub const BIAS: &str = "BIAS";
    pub const DARK: &str = "DARK";
    pub const MASTER_FLAT: &str = "MASTERFLAT";
    pub const MASTER_DARK: &str = "MASTERDARK";
    pub const MASTER_BIAS: &str = "MASTERBIAS";
    pub const MASTER_DARKFLAT: &str = "MASTERDARKFLAT";
    pub const DARK_FLAT: &str = "DARKFLAT";
}

/// `_KNOWN_OVERRIDE_TARGETS`: every `InternalColumns` value, lower-cased.
///
/// The loader warns when an `[override]` or `[equipmentoverrides]` target is
/// not one of these, because such an entry silently creates a phantom column
/// nothing consumes -- the shipped config's `SWCREATOR = CREATOR` ('swcreator',
/// not 'swcreate') has never done anything. Held sorted, since the warning
/// interpolates `', '.join(sorted(...))` verbatim.
pub const KNOWN_OVERRIDE_TARGETS: [&str; 38] = [
    "bortle",
    "ccd-temp",
    "date-obs",
    "egain",
    "end_date",
    "exposure",
    "filename",
    "filter",
    "focallen",
    "focname",
    "focratio",
    "foctemp",
    "fwheel",
    "fwhm",
    "gain",
    "gain_match",
    "hfr",
    "imagetyp",
    "imscale",
    "instrume",
    "num_days",
    "number",
    "object",
    "rotantang",
    "rotname",
    "sessions",
    "site",
    "sitelat",
    "sitelong",
    "source_path",
    "sqm",
    "start_date",
    "swcreate",
    "telescop",
    "temp_max",
    "temp_min",
    "xbinning",
    "xpixsz",
];
