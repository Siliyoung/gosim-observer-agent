/**
 * Sky geometry: the same public formulas the engine uses (see docs/participant-guide.zh.md section 2-3),
 * so pointings planned here line up with real fibre hits. All angles in degrees.
 * Azimuth: 0 = north, 90 = east. No external dependencies.
 */

export const SIDEREAL_DEG_PER_SECOND = 360.98564736629 / 86400.0;

const DEG = Math.PI / 180.0;
const RAD = 180.0 / Math.PI;

export function parseUtc(value: string): Date {
  return new Date(value);
}

export function formatUtc(moment: Date): string {
  return moment.toISOString().slice(0, 19) + "Z";
}

export function julianDate(moment: Date): number {
  return moment.getTime() / 1000.0 / 86400.0 + 2440587.5;
}

export function localSiderealDeg(moment: Date, longitudeDeg: number): number {
  const days = julianDate(moment) - 2451545.0;
  return mod360(280.46061837 + 360.98564736629 * days + longitudeDeg);
}

export function mod360(angle: number): number {
  const m = angle % 360.0;
  return m < 0 ? m + 360.0 : m;
}

export function wrap180(angle: number): number {
  return mod360(angle + 180.0) - 180.0;
}

function clamp(value: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, value));
}

export function radecToAltAz(
  raDeg: number,
  decDeg: number,
  lstDeg: number,
  latitudeDeg: number
): [number, number] {
  const hourAngle = wrap180(lstDeg - raDeg) * DEG;
  const lat = latitudeDeg * DEG;
  const dec = decDeg * DEG;
  const sinAlt = Math.sin(lat) * Math.sin(dec) + Math.cos(lat) * Math.cos(dec) * Math.cos(hourAngle);
  const alt = Math.asin(clamp(sinAlt, -1.0, 1.0));
  const cosAlt = Math.max(1e-12, Math.cos(alt));
  const sinAz = (-Math.sin(hourAngle) * Math.cos(dec)) / cosAlt;
  const cosAz = (Math.sin(dec) - Math.sin(alt) * Math.sin(lat)) / (cosAlt * Math.max(1e-12, Math.cos(lat)));
  return [alt * RAD, mod360(Math.atan2(sinAz, cosAz) * RAD)];
}

export function altazToRadec(
  altDeg: number,
  azDeg: number,
  lstDeg: number,
  latitudeDeg: number
): [number, number] {
  const alt = altDeg * DEG;
  const az = azDeg * DEG;
  const lat = latitudeDeg * DEG;
  const sinDec = Math.sin(alt) * Math.sin(lat) + Math.cos(alt) * Math.cos(lat) * Math.cos(az);
  const dec = Math.asin(clamp(sinDec, -1.0, 1.0));
  const cosDec = Math.max(1e-12, Math.cos(dec));
  const sinH = (-Math.sin(az) * Math.cos(alt)) / cosDec;
  const cosH = (Math.sin(alt) - Math.sin(dec) * Math.sin(lat)) / (cosDec * Math.max(1e-12, Math.cos(lat)));
  const hour = Math.atan2(sinH, cosH) * RAD;
  return [mod360(lstDeg - hour), dec * RAD];
}

/** Largest |hour angle| (deg) at which a source stays at or above minAlt (0 = never, 180 = always). */
export function maxHourAngleDeg(decDeg: number, latitudeDeg: number, minAltDeg: number): number {
  const lat = latitudeDeg * DEG;
  const dec = decDeg * DEG;
  const denominator = Math.cos(lat) * Math.cos(dec);
  if (Math.abs(denominator) < 1e-12) return 0.0;
  const value = (Math.sin(minAltDeg * DEG) - Math.sin(lat) * Math.sin(dec)) / denominator;
  if (value >= 1.0) return 0.0;
  if (value <= -1.0) return 180.0;
  return Math.acos(value) * RAD;
}

/** (north, east) gnomonic offsets in degrees of a target from a field centre, or null if behind the plane. */
export function tangentOffsets(
  targetAlt: number,
  targetAz: number,
  centerAlt: number,
  centerAz: number
): [number, number] | null {
  const alt = targetAlt * DEG;
  const az = targetAz * DEG;
  const calt = centerAlt * DEG;
  const caz = centerAz * DEG;
  const t: [number, number, number] = [Math.cos(alt) * Math.cos(az), Math.cos(alt) * Math.sin(az), Math.sin(alt)];
  const c: [number, number, number] = [Math.cos(calt) * Math.cos(caz), Math.cos(calt) * Math.sin(caz), Math.sin(calt)];
  const north: [number, number, number] = [-Math.sin(calt) * Math.cos(caz), -Math.sin(calt) * Math.sin(caz), Math.cos(calt)];
  const east: [number, number, number] = [-Math.sin(caz), Math.cos(caz), 0.0];
  const depth = t[0] * c[0] + t[1] * c[1] + t[2] * c[2];
  if (depth <= 0.0) return null;
  const dotNorth = t[0] * north[0] + t[1] * north[1] + t[2] * north[2];
  const dotEast = t[0] * east[0] + t[1] * east[1] + t[2] * east[2];
  return [(dotNorth / depth) * RAD, (dotEast / depth) * RAD];
}

/** The point d_north / d_east degrees away on the tangent plane at (alt, az). Works near the zenith. */
export function shiftAltaz(altDeg: number, azDeg: number, dNorth: number, dEast: number): [number, number] {
  const alt = altDeg * DEG;
  const az = azDeg * DEG;
  const point: [number, number, number] = [Math.cos(alt) * Math.cos(az), Math.cos(alt) * Math.sin(az), Math.sin(alt)];
  const north: [number, number, number] = [-Math.sin(alt) * Math.cos(az), -Math.sin(alt) * Math.sin(az), Math.cos(alt)];
  const east: [number, number, number] = [-Math.sin(az), Math.cos(az), 0.0];
  const dn = dNorth * DEG;
  const de = dEast * DEG;
  const x = point[0] + dn * north[0] + de * east[0];
  const y = point[1] + dn * north[1] + de * east[1];
  const z = point[2] + dn * north[2] + de * east[2];
  const norm = Math.sqrt(x * x + y * y + z * z);
  const xn = x / norm;
  const yn = y / norm;
  const zn = z / norm;
  return [Math.asin(clamp(zn, -1.0, 1.0)) * RAD, mod360(Math.atan2(yn, xn) * RAD)];
}

/** n x n square fibres; fibre 0 bottom-left, rows along +alt, columns along +az. */
export class FiberGrid {
  readonly side: number;
  readonly n: number;
  readonly glass: number;
  readonly pitch: number;
  readonly fov: number;

  constructor(instrument: { grid_side: number; n_fibers: number; glass_side_deg: number; pitch_deg: number; fov_side_deg: number }) {
    this.side = instrument.grid_side;
    this.n = instrument.n_fibers;
    this.glass = instrument.glass_side_deg;
    this.pitch = instrument.pitch_deg;
    this.fov = instrument.fov_side_deg;
  }

  fiberCenter(fiber: number): [number, number] {
    const row = Math.floor(fiber / this.side);
    const col = fiber % this.side;
    const middle = (this.side - 1) / 2.0;
    return [(row - middle) * this.pitch, (col - middle) * this.pitch];
  }

  /** -> [fiber id, margin in degrees to the glass edge] or [null, negative] when not on glass. */
  classify(dNorth: number, dEast: number): [number | null, number] {
    const half = this.fov / 2.0;
    if (Math.abs(dNorth) > half || Math.abs(dEast) > half) return [null, -1.0];
    const middle = this.side / 2.0;
    const row = Math.min(Math.max(Math.floor(dNorth / this.pitch + middle), 0), this.side - 1);
    const col = Math.min(Math.max(Math.floor(dEast / this.pitch + middle), 0), this.side - 1);
    const fiber = row * this.side + col;
    const [cNorth, cEast] = this.fiberCenter(fiber);
    const margin = this.glass / 2.0 - Math.max(Math.abs(dNorth - cNorth), Math.abs(dEast - cEast));
    return margin >= 0.0 ? [fiber, margin] : [null, margin];
  }
}

export function normalizedAirmass(altDeg: number): number {
  if (altDeg <= 0.0) return Infinity;
  const zenith = 90.0 - altDeg;
  const raw = 1.0 / (Math.cos(zenith * DEG) + 0.50572 * Math.pow(96.07995 - zenith, -1.6364));
  return raw / (1.0 / (1.0 + 0.50572 * Math.pow(96.07995, -1.6364)));
}

export function sunRadec(moment: Date): [number, number] {
  const days = julianDate(moment) - 2451545.0;
  const meanLongitude = mod360(280.46 + 0.9856474 * days);
  const anomaly = mod360(357.528 + 0.9856003 * days) * DEG;
  const longitude = mod360(meanLongitude + 1.915 * Math.sin(anomaly) + 0.02 * Math.sin(2 * anomaly)) * DEG;
  const obliquity = (23.439 - 0.0000004 * days) * DEG;
  const ra = mod360(Math.atan2(Math.cos(obliquity) * Math.sin(longitude), Math.cos(longitude)) * RAD);
  const dec = Math.asin(Math.sin(obliquity) * Math.sin(longitude)) * RAD;
  return [ra, dec];
}

export function moonRadec(moment: Date): [number, number] {
  const days = julianDate(moment) - 2451545.0;
  const meanLongitude = mod360(218.316 + 13.176396 * days) * DEG;
  const anomaly = mod360(134.963 + 13.064993 * days) * DEG;
  const argLatitude = mod360(93.272 + 13.22935 * days) * DEG;
  const longitude = meanLongitude + 6.289 * DEG * Math.sin(anomaly);
  const latitude = 5.128 * DEG * Math.sin(argLatitude);
  const obliquity = (23.439 - 0.0000004 * days) * DEG;
  const x = Math.cos(longitude) * Math.cos(latitude);
  const y = Math.sin(longitude) * Math.cos(latitude) * Math.cos(obliquity) - Math.sin(latitude) * Math.sin(obliquity);
  const z = Math.sin(longitude) * Math.cos(latitude) * Math.sin(obliquity) + Math.sin(latitude) * Math.cos(obliquity);
  return [mod360(Math.atan2(y, x) * RAD), Math.asin(clamp(z, -1.0, 1.0)) * RAD];
}

export function separationDeg(ra1: number, dec1: number, ra2: number, dec2: number): number {
  const r1 = ra1 * DEG;
  const d1 = dec1 * DEG;
  const r2 = ra2 * DEG;
  const d2 = dec2 * DEG;
  const cosine = Math.sin(d1) * Math.sin(d2) + Math.cos(d1) * Math.cos(d2) * Math.cos(r1 - r2);
  return Math.acos(clamp(cosine, -1.0, 1.0)) * RAD;
}

/** Moon state at one instant (position + illumination); the public lunar_factor formula lives in scoring.ts. */
export class MoonState {
  readonly ra: number;
  readonly dec: number;
  readonly illumination: number;
  readonly alt: number;

  constructor(moment: Date, lstDeg: number, latitudeDeg: number) {
    const [ra, dec] = moonRadec(moment);
    this.ra = ra;
    this.dec = dec;
    const [sunRa, sunDec] = sunRadec(moment);
    this.illumination = (1.0 - Math.cos(separationDeg(sunRa, sunDec, ra, dec) * DEG)) / 2.0;
    const [alt] = radecToAltAz(ra, dec, lstDeg, latitudeDeg);
    this.alt = alt;
  }
}
