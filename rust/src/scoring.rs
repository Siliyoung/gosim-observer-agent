//! Sky geometry and the public scoring formulas (participant guide, protocol
//! section 8 / Appendix B) -- reimplemented from the spec, not from any
//! hidden table. The hidden per-slot weather (transparency, seeing, cloud,
//! instrument efficiency) is never available; `memory::Memory` stands in for
//! it with a single learned scale, nudged from the agent's own hits.

use std::f64::consts::PI;

pub const SIDEREAL_DEG_PER_SECOND: f64 = 360.98564736629 / 86400.0;

fn rad(deg: f64) -> f64 {
    deg * PI / 180.0
}

fn deg(rad: f64) -> f64 {
    rad * 180.0 / PI
}

pub fn wrap360(deg: f64) -> f64 {
    let m = deg % 360.0;
    if m < 0.0 {
        m + 360.0
    } else {
        m
    }
}

pub fn wrap180(deg: f64) -> f64 {
    wrap360(deg + 180.0) - 180.0
}

/// Days since the Unix epoch (1970-01-01T00:00:00Z) for a UTC civil date/time,
/// via Howard Hinnant's days_from_civil algorithm (proleptic Gregorian, no
/// library dependency). Returns `None` for an out-of-range month/day.
fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11], Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    Some(era * 146097 + doe - 719468)
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff]Z` into Unix seconds (UTC). Returns `None`
/// on any format mismatch -- callers must treat that as "unusable timestamp",
/// never panic on a message the backend sent.
pub fn parse_utc(value: &str) -> Option<f64> {
    let v = value.strip_suffix('Z')?;
    let (date, time) = v.split_once('T')?;
    let mut date_parts = date.split('-');
    let y: i64 = date_parts.next()?.parse().ok()?;
    let m: u32 = date_parts.next()?.parse().ok()?;
    let d: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let mut time_parts = time.split(':');
    let hh: f64 = time_parts.next()?.parse().ok()?;
    let mm: f64 = time_parts.next()?.parse().ok()?;
    let ss: f64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }
    let days = days_from_civil(y, m, d)?;
    Some(days as f64 * 86400.0 + hh * 3600.0 + mm * 60.0 + ss)
}

/// Formats Unix seconds back into `YYYY-MM-DDTHH:MM:SSZ` (whole seconds,
/// truncating towards the past). Used for `wait.until_utc` -- sleeping
/// straight through to the next observing night in one response instead of
/// polling every exposure-length `wait`.
pub fn format_utc(unix_seconds: f64) -> String {
    let total = unix_seconds.floor() as i64;
    let mut days = total.div_euclid(86400);
    let mut secs_of_day = total.rem_euclid(86400);
    let hh = secs_of_day / 3600;
    secs_of_day -= hh * 3600;
    let mm = secs_of_day / 60;
    let ss = secs_of_day - mm * 60;
    // civil_from_days, the inverse of days_from_civil.
    days += 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn julian_date(unix_seconds: f64) -> f64 {
    unix_seconds / 86400.0 + 2440587.5
}

/// Greenwich-plus-longitude sidereal time, in degrees. Same low-order
/// approximation the guide's worked examples use; good to a few arcseconds
/// over the survey's time span, well inside fibre-pitch tolerance.
pub fn local_sidereal_deg(unix_seconds: f64, longitude_deg: f64) -> f64 {
    let days = julian_date(unix_seconds) - 2451545.0;
    wrap360(280.46061837 + 360.98564736629 * days + longitude_deg)
}

/// Equatorial (RA/Dec) -> horizontal (alt/az) at a given local sidereal time.
/// Azimuth is measured from north through east, matching the guide's layout note.
pub fn radec_to_altaz(ra_deg: f64, dec_deg: f64, lst_deg: f64, latitude_deg: f64) -> (f64, f64) {
    let hour_angle = rad(wrap180(lst_deg - ra_deg));
    let lat = rad(latitude_deg);
    let dec = rad(dec_deg);
    let sin_alt = lat.sin() * dec.sin() + lat.cos() * dec.cos() * hour_angle.cos();
    let alt = sin_alt.clamp(-1.0, 1.0).asin();
    let cos_alt = alt.cos().max(1e-12);
    let sin_az = -hour_angle.sin() * dec.cos() / cos_alt;
    let cos_az = (dec.sin() - alt.sin() * lat.sin()) / (cos_alt * lat.cos().max(1e-12));
    (deg(alt), wrap360(deg(sin_az.atan2(cos_az))))
}

/// The inverse of `radec_to_altaz`: horizontal -> equatorial at a given LST.
pub fn altaz_to_radec(alt_deg: f64, az_deg: f64, lst_deg: f64, latitude_deg: f64) -> (f64, f64) {
    let alt = rad(alt_deg);
    let az = rad(az_deg);
    let lat = rad(latitude_deg);
    let sin_dec = alt.sin() * lat.sin() + alt.cos() * lat.cos() * az.cos();
    let dec = sin_dec.clamp(-1.0, 1.0).asin();
    let cos_dec = dec.cos().max(1e-12);
    let sin_h = -az.sin() * alt.cos() / cos_dec;
    let cos_h = (alt.sin() - dec.sin() * lat.sin()) / (cos_dec * lat.cos().max(1e-12));
    let hour = deg(sin_h.atan2(cos_h));
    (wrap360(lst_deg - hour), deg(dec))
}

/// Largest `|hour angle|` (deg) at which a source stays at or above
/// `min_alt_deg` (0 = never visible, 180 = always above the limit).
pub fn max_hour_angle_deg(dec_deg: f64, latitude_deg: f64, min_alt_deg: f64) -> f64 {
    let lat = rad(latitude_deg);
    let dec = rad(dec_deg);
    let denominator = lat.cos() * dec.cos();
    if denominator.abs() < 1e-12 {
        return 0.0;
    }
    let value = (rad(min_alt_deg).sin() - lat.sin() * dec.sin()) / denominator;
    if value >= 1.0 {
        0.0
    } else if value <= -1.0 {
        180.0
    } else {
        deg(value.acos())
    }
}

/// Gnomonic (north, east) offset in degrees of a target from a field centre,
/// or `None` when the target is on the far side of the tangent plane.
pub fn tangent_offsets(target_alt: f64, target_az: f64, center_alt: f64, center_az: f64) -> Option<(f64, f64)> {
    let (alt, az) = (rad(target_alt), rad(target_az));
    let (calt, caz) = (rad(center_alt), rad(center_az));
    let t = (alt.cos() * az.cos(), alt.cos() * az.sin(), alt.sin());
    let c = (calt.cos() * caz.cos(), calt.cos() * caz.sin(), calt.sin());
    let north = (-calt.sin() * caz.cos(), -calt.sin() * caz.sin(), calt.cos());
    let east = (-caz.sin(), caz.cos(), 0.0);
    let depth = t.0 * c.0 + t.1 * c.1 + t.2 * c.2;
    if depth <= 0.0 {
        return None;
    }
    let n = (t.0 * north.0 + t.1 * north.1 + t.2 * north.2) / depth;
    let e = (t.0 * east.0 + t.1 * east.1 + t.2 * east.2) / depth;
    Some((deg(n), deg(e)))
}

/// The point `d_north`/`d_east` degrees away on the tangent plane at
/// `(alt, az)`. Used to ask "if I centred the field on fibre `f` instead,
/// where would the pointing itself have to be?" -- the core move of the
/// anchor search in `planner.rs`.
pub fn shift_altaz(alt_deg: f64, az_deg: f64, d_north: f64, d_east: f64) -> (f64, f64) {
    let alt = rad(alt_deg);
    let az = rad(az_deg);
    let point = (alt.cos() * az.cos(), alt.cos() * az.sin(), alt.sin());
    let north = (-alt.sin() * az.cos(), -alt.sin() * az.sin(), alt.cos());
    let east = (-az.sin(), az.cos(), 0.0);
    let dn = rad(d_north);
    let de = rad(d_east);
    let x = point.0 + dn * north.0 + de * east.0;
    let y = point.1 + dn * north.1 + de * east.1;
    let z = point.2 + dn * north.2 + de * east.2;
    let norm = (x * x + y * y + z * z).sqrt();
    (deg((z / norm).clamp(-1.0, 1.0).asin()), wrap360(deg((y / norm).atan2(x / norm))))
}

/// Airmass relative to zenith (1.0 at `alt=90`), Kasten-Young model.
/// Returns `f64::INFINITY` at or below the horizon.
pub fn normalized_airmass(alt_deg: f64) -> f64 {
    if alt_deg <= 0.0 {
        return f64::INFINITY;
    }
    let zenith = 90.0 - alt_deg;
    let raw = 1.0 / (rad(zenith).cos() + 0.50572 * (96.07995 - zenith).powf(-1.6364));
    let zenith_ref = 96.07995_f64.powf(-1.6364);
    raw / (1.0 / (1.0 + 0.50572 * zenith_ref))
}

fn sun_radec(unix_seconds: f64) -> (f64, f64) {
    let days = julian_date(unix_seconds) - 2451545.0;
    let mean_longitude = wrap360(280.460 + 0.9856474 * days);
    let anomaly = rad(wrap360(357.528 + 0.9856003 * days));
    let longitude = rad(wrap360(mean_longitude + 1.915 * anomaly.sin() + 0.020 * (2.0 * anomaly).sin()));
    let obliquity = rad(23.439 - 0.0000004 * days);
    let ra = wrap360(deg((obliquity.cos() * longitude.sin()).atan2(longitude.cos())));
    let dec = deg((obliquity.sin() * longitude.sin()).asin());
    (ra, dec)
}

fn moon_radec(unix_seconds: f64) -> (f64, f64) {
    let days = julian_date(unix_seconds) - 2451545.0;
    let mean_longitude = rad(wrap360(218.316 + 13.176396 * days));
    let anomaly = rad(wrap360(134.963 + 13.064993 * days));
    let arg_latitude = rad(wrap360(93.272 + 13.229350 * days));
    let longitude = mean_longitude + rad(6.289) * anomaly.sin();
    let latitude = rad(5.128) * arg_latitude.sin();
    let obliquity = rad(23.439 - 0.0000004 * days);
    let x = longitude.cos() * latitude.cos();
    let y = longitude.sin() * latitude.cos() * obliquity.cos() - latitude.sin() * obliquity.sin();
    let z = longitude.sin() * latitude.cos() * obliquity.sin() + latitude.sin() * obliquity.cos();
    (wrap360(deg(y.atan2(x))), deg(z.clamp(-1.0, 1.0).asin()))
}

pub fn separation_deg(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let (r1, d1, r2, d2) = (rad(ra1), rad(dec1), rad(ra2), rad(dec2));
    let cosine = d1.sin() * d2.sin() + d1.cos() * d2.cos() * (r1 - r2).cos();
    deg(cosine.clamp(-1.0, 1.0).acos())
}

#[derive(Clone, Copy, Debug)]
pub struct LunarModel {
    pub maximum_penalty: f64,
    pub altitude_exponent: f64,
    pub angular_decay_scale_deg: f64,
}

impl Default for LunarModel {
    fn default() -> Self {
        LunarModel {
            maximum_penalty: 0.5,
            altitude_exponent: 1.0,
            angular_decay_scale_deg: 40.0,
        }
    }
}

/// Moon position and illumination at one instant, with the public lunar
/// penalty model from `initialize.payload.scoring.lunar_model` (formula 14).
pub struct Moon {
    ra_deg: f64,
    dec_deg: f64,
    illumination: f64,
    pub alt_deg: f64,
    model: LunarModel,
}

impl Moon {
    pub fn at(unix_seconds: f64, latitude_deg: f64, longitude_deg: f64, model: LunarModel) -> Moon {
        let (ra, dec) = moon_radec(unix_seconds);
        let (sun_ra, sun_dec) = sun_radec(unix_seconds);
        let illumination = (1.0 - rad(separation_deg(sun_ra, sun_dec, ra, dec)).cos()) / 2.0;
        let lst = local_sidereal_deg(unix_seconds, longitude_deg);
        let (alt, _az) = radec_to_altaz(ra, dec, lst, latitude_deg);
        Moon {
            ra_deg: ra,
            dec_deg: dec,
            illumination,
            alt_deg: alt,
            model,
        }
    }

    /// 1.0 = no lunar penalty, lower = more light pollution at that sky position.
    pub fn lunar_factor(&self, ra_deg: f64, dec_deg: f64) -> f64 {
        if self.alt_deg <= 0.0 {
            return 1.0;
        }
        let separation = separation_deg(ra_deg, dec_deg, self.ra_deg, self.dec_deg);
        let penalty = self.model.maximum_penalty
            * self.illumination
            * rad(self.alt_deg).sin().max(0.0).powf(self.model.altitude_exponent)
            * (-separation / self.model.angular_decay_scale_deg.max(1e-6)).exp();
        (1.0 - penalty).clamp(0.0, 1.0)
    }
}

/// `grid_side` x `grid_side` square fibres; fibre 0 bottom-left, rows along
/// +altitude, columns along +azimuth in the pointing's own tangent plane
/// (participant guide, Geometry section).
pub struct FiberGrid {
    pub side: i64,
    pub glass_deg: f64,
    pub pitch_deg: f64,
    pub fov_deg: f64,
}

impl FiberGrid {
    pub fn n_fibers(&self) -> i64 {
        self.side * self.side
    }

    pub fn fiber_center(&self, fiber: i64) -> (f64, f64) {
        let row = fiber / self.side;
        let col = fiber % self.side;
        let middle = (self.side - 1) as f64 / 2.0;
        ((row as f64 - middle) * self.pitch_deg, (col as f64 - middle) * self.pitch_deg)
    }

    /// `(fiber id, margin in degrees to the glass edge)`, or `(None, negative
    /// margin)` when the offset does not land on any fibre's glass.
    pub fn classify(&self, d_north: f64, d_east: f64) -> (Option<i64>, f64) {
        let half = self.fov_deg / 2.0;
        if d_north.abs() > half || d_east.abs() > half {
            return (None, -1.0);
        }
        let middle = self.side as f64 / 2.0;
        let row = ((d_north / self.pitch_deg + middle).floor() as i64).clamp(0, self.side - 1);
        let col = ((d_east / self.pitch_deg + middle).floor() as i64).clamp(0, self.side - 1);
        let fiber = row * self.side + col;
        let (c_north, c_east) = self.fiber_center(fiber);
        let margin = self.glass_deg / 2.0 - (d_north - c_north).abs().max((d_east - c_east).abs());
        if margin >= 0.0 {
            (Some(fiber), margin)
        } else {
            (None, margin)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProgramConfig {
    pub band_dark: f64,
    pub band_bright: f64,
    pub multiplier_dark: f64,
    pub multiplier_bright: f64,
    pub multiplier_backup: f64,
    pub mismatch_multiplier: f64,
}

impl Default for ProgramConfig {
    fn default() -> Self {
        ProgramConfig {
            band_dark: 0.65,
            band_bright: 0.40,
            multiplier_dark: 1.20,
            multiplier_bright: 1.12,
            multiplier_backup: 1.06,
            mismatch_multiplier: 1.0,
        }
    }
}

impl ProgramConfig {
    pub fn multiplier_for(&self, program: &str) -> f64 {
        match program {
            "DARK" => self.multiplier_dark,
            "BRIGHT" => self.multiplier_bright,
            _ => self.multiplier_backup,
        }
    }

    pub fn richest_multiplier(&self) -> f64 {
        self.multiplier_dark.max(self.multiplier_bright).max(self.multiplier_backup)
    }
}

/// A rough per-second quality model for a target: lunar factor over
/// `q0 * airmass^beta` (formulas 17/19 of the guide). Callers multiply by the
/// agent's own learned `scale` for the hidden weather terms separately
/// (`memory::Memory::scale`) -- kept apart here because a few call sites
/// (e.g. the back-calculation in `Memory::on_result`) need the unscaled model.
pub fn quality_model(alt_deg: f64, lunar: f64, q0: f64, airmass_exponent: f64) -> f64 {
    lunar / (q0.max(1e-9) * normalized_airmass(alt_deg.max(1.0)).powf(airmass_exponent))
}

/// Which program band a quality ratio (relative to `scoring.program.bands`) falls into.
pub fn program_band(q_band: f64, program: &ProgramConfig) -> &'static str {
    if q_band >= program.band_dark {
        "DARK"
    } else if q_band >= program.band_bright {
        "BRIGHT"
    } else {
        "BACKUP"
    }
}

/// Compass direction -> azimuth degrees, for interpreting bulletin/forecast
/// notices ("avoid the NE") and LLM night advice in the same terms.
pub fn direction_azimuth(direction: &str) -> Option<f64> {
    match direction {
        "N" => Some(0.0),
        "NE" => Some(45.0),
        "E" => Some(90.0),
        "SE" => Some(135.0),
        "S" => Some(180.0),
        "SW" => Some(225.0),
        "W" => Some(270.0),
        "NW" => Some(315.0),
        _ => None,
    }
}

pub fn az_distance(a: f64, b: f64) -> f64 {
    wrap180(a - b).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utc_timestamps() {
        // 2026-10-02T00:00:00Z, cross-checked against a known Unix epoch offset.
        assert_eq!(parse_utc("2026-10-02T00:00:00Z"), Some(1_790_899_200.0));
        assert_eq!(parse_utc("not-a-timestamp"), None);
    }

    #[test]
    fn roundtrips_utc_timestamps() {
        let s = "2026-10-02T00:00:00Z";
        assert_eq!(format_utc(parse_utc(s).unwrap()), s);
    }

    #[test]
    fn zenith_airmass_is_one() {
        assert!((normalized_airmass(90.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fiber_grid_center_lands_on_fiber_zero_region() {
        let grid = FiberGrid {
            side: 4,
            glass_deg: 0.632456,
            pitch_deg: 0.632456,
            fov_deg: 2.529822,
        };
        let (fiber, margin) = grid.classify(0.0, 0.0);
        assert!(fiber.is_some());
        assert!(margin >= 0.0);
    }

    #[test]
    fn altaz_radec_roundtrip() {
        let (alt, az) = radec_to_altaz(120.0, -10.0, 42.0, -24.6);
        let (ra, dec) = altaz_to_radec(alt, az, 42.0, -24.6);
        assert!((wrap180(ra - 120.0)).abs() < 1e-6);
        assert!((dec - -10.0).abs() < 1e-6);
    }

    #[test]
    fn max_hour_angle_is_zero_when_never_visible() {
        // Far-southern declination, northern-ish latitude: never above a high limit.
        assert_eq!(max_hour_angle_deg(-80.0, 40.0, 30.0), 0.0);
    }
}
