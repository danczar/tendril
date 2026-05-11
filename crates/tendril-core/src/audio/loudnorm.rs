use serde::Deserialize;

/// Measured loudness values reported by ffmpeg's `loudnorm` pass-1
/// (`print_format=json`). Fed into pass-2's `measured_*` parameters so the
/// final encode does a one-shot linear gain adjustment instead of dynamic
/// compression, yielding mastering-grade accuracy at the target LUFS.
///
/// loudnorm emits numeric fields as JSON *strings* (`"input_i" : "-9.45"`),
/// so each field uses a `parse_f32_str` deserializer.
#[derive(Debug, Clone, Deserialize)]
pub struct LoudnormMeasurement {
    #[serde(deserialize_with = "parse_f32_str")]
    pub input_i: f32,
    #[serde(deserialize_with = "parse_f32_str")]
    pub input_lra: f32,
    #[serde(deserialize_with = "parse_f32_str")]
    pub input_tp: f32,
    #[serde(deserialize_with = "parse_f32_str")]
    pub input_thresh: f32,
    #[serde(deserialize_with = "parse_f32_str")]
    pub target_offset: f32,
}

fn parse_f32_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

/// Single-pass loudnorm filter. Used as a fallback when pass-1 measurement
/// fails (e.g. silent input, parse error). Lands within ~1 LU of target on
/// typical music.
pub fn loudnorm_filter(target_lufs: f32) -> String {
    format!("loudnorm=I={target_lufs}:LRA=11:TP=-1.5")
}

/// Pass-1 loudnorm filter: measures the input and prints a JSON summary to
/// stderr. Output of the filter is discarded (via `-f null -` at the caller).
pub fn measure_filter(target_lufs: f32) -> String {
    format!("loudnorm=I={target_lufs}:LRA=11:TP=-1.5:print_format=json")
}

/// Pass-2 loudnorm filter: applies a linear gain adjustment using the
/// measured values from pass-1. `linear=true` is the bit that makes this
/// mastering-grade — without it, the second pass still runs the dynamic
/// compressor and you only gain a small accuracy improvement.
pub fn apply_filter(target_lufs: f32, m: &LoudnormMeasurement) -> String {
    format!(
        "loudnorm=I={target_lufs}:LRA=11:TP=-1.5:\
         measured_I={input_i}:measured_LRA={input_lra}:measured_TP={input_tp}:\
         measured_thresh={input_thresh}:offset={offset}:linear=true",
        input_i = m.input_i,
        input_lra = m.input_lra,
        input_tp = m.input_tp,
        input_thresh = m.input_thresh,
        offset = m.target_offset,
    )
}

/// Extract the loudnorm JSON object from ffmpeg stderr.
///
/// loudnorm prints one JSON block near the end of stderr (after all
/// progress logging). We scan from the back for the last `{ ... }` pair,
/// which is reliable because the JSON has no nested objects.
///
/// Returns `None` if any measurement is non-finite. Rust's `f32::FromStr`
/// happily parses `"-inf"` (loudnorm emits this for silent input), but
/// feeding `-inf` into the pass-2 filter string would produce garbage —
/// caller falls back to single-pass loudnorm in that case.
pub fn parse_measurement(stderr: &str) -> Option<LoudnormMeasurement> {
    let start = stderr.rfind('{')?;
    let end = stderr.rfind('}')?;
    if end <= start {
        return None;
    }
    let m: LoudnormMeasurement = serde_json::from_str(&stderr[start..=end]).ok()?;
    let all_finite = m.input_i.is_finite()
        && m.input_lra.is_finite()
        && m.input_tp.is_finite()
        && m.input_thresh.is_finite()
        && m.target_offset.is_finite();
    all_finite.then_some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudnorm_filter_contains_target_lufs() {
        assert!(loudnorm_filter(-14.0).contains("I=-14"));
        assert!(loudnorm_filter(-23.0).contains("I=-23"));
    }

    #[test]
    fn loudnorm_filter_uses_ebu_r128_lra_and_tp() {
        let f = loudnorm_filter(-14.0);
        assert!(f.contains("LRA=11"), "got: {f}");
        assert!(f.contains("TP=-1.5"), "got: {f}");
    }

    #[test]
    fn measure_filter_requests_json_output() {
        let f = measure_filter(-14.0);
        assert!(f.contains("print_format=json"), "got: {f}");
    }

    #[test]
    fn apply_filter_passes_measured_values_and_enables_linear() {
        let m = LoudnormMeasurement {
            input_i: -9.45,
            input_lra: 9.10,
            input_tp: -2.28,
            input_thresh: -19.80,
            target_offset: 0.65,
        };
        let f = apply_filter(-14.0, &m);
        assert!(f.contains("measured_I=-9.45"), "got: {f}");
        assert!(f.contains("measured_LRA=9.1"), "got: {f}");
        assert!(f.contains("measured_TP=-2.28"), "got: {f}");
        assert!(f.contains("measured_thresh=-19.8"), "got: {f}");
        assert!(f.contains("offset=0.65"), "got: {f}");
        assert!(f.contains("linear=true"), "got: {f}");
        assert!(
            !f.contains("print_format"),
            "pass-2 must not request JSON output, got: {f}"
        );
    }

    /// Realistic loudnorm stderr blob with the JSON at the end (matches
    /// ffmpeg 7.x output format).
    const SAMPLE_STDERR: &str = r#"
ffmpeg version 7.1.1 Copyright (c) 2000-2025 the FFmpeg developers
[Parsed_loudnorm_0 @ 0x600003204000] Stream level: -9.45 LUFS
size=N/A time=00:03:42.10 bitrate=N/A speed= 18x
[Parsed_loudnorm_0 @ 0x600003204000]
{
	"input_i" : "-9.45",
	"input_tp" : "-2.28",
	"input_lra" : "9.10",
	"input_thresh" : "-19.80",
	"output_i" : "-14.65",
	"output_tp" : "-2.96",
	"output_lra" : "8.20",
	"output_thresh" : "-25.04",
	"normalization_type" : "dynamic",
	"target_offset" : "0.65"
}
"#;

    #[test]
    fn parse_measurement_extracts_all_input_fields() {
        let m = parse_measurement(SAMPLE_STDERR).expect("parse");
        assert!((m.input_i - -9.45).abs() < 1e-4);
        assert!((m.input_tp - -2.28).abs() < 1e-4);
        assert!((m.input_lra - 9.10).abs() < 1e-4);
        assert!((m.input_thresh - -19.80).abs() < 1e-4);
        assert!((m.target_offset - 0.65).abs() < 1e-4);
    }

    #[test]
    fn parse_measurement_returns_none_when_no_json() {
        assert!(parse_measurement("").is_none());
        assert!(parse_measurement("ffmpeg version 7.1.1 nothing else").is_none());
        // braces in the wrong order still rejected
        assert!(parse_measurement("} prefix garbage {").is_none());
    }

    #[test]
    fn parse_measurement_returns_none_on_inf_values() {
        // Silent audio produces "-inf" string, which doesn't parse as f32.
        // Caller should treat None as "fall back to single-pass".
        let stderr_inf = r#"
[Parsed_loudnorm_0 @ 0x1]
{
	"input_i" : "-inf",
	"input_tp" : "-inf",
	"input_lra" : "0.00",
	"input_thresh" : "-inf",
	"output_i" : "-inf",
	"target_offset" : "0.00"
}
"#;
        assert!(parse_measurement(stderr_inf).is_none());
    }
}
