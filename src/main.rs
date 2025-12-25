use anyhow::{bail, Context, Result};
use hound::{SampleFormat, WavReader};
use std::{env, fs::File, io::Write, path::Path};

const TARGET_SR: u32 = 48_000;
const TARGET_SAMPLES: usize = 1024;

/// Read 24-bit PCM samples from WAV into i32 (sign-extended), one per frame.
fn read_pcm24_mono(path: &Path) -> Result<Vec<i32>> {
    let mut reader = WavReader::open(path).with_context(|| format!("open {:?}", path))?;
    let spec = reader.spec();

    if spec.sample_rate != TARGET_SR {
        bail!("Expected {} Hz, got {}", TARGET_SR, spec.sample_rate);
    }
    if spec.channels != 1 {
        bail!("Expected mono (1 channel), got {}", spec.channels);
    }
    if spec.sample_format != SampleFormat::Int {
        bail!("Expected integer PCM, got {:?}", spec.sample_format);
    }
    if spec.bits_per_sample != 24 {
        bail!("Expected 24-bit PCM, got {}-bit", spec.bits_per_sample);
    }

    // hound returns i32 for 24-bit samples (already sign-extended).
    let mut out = Vec::new();
    for s in reader.samples::<i32>() {
        out.push(s?);
    }
    Ok(out)
}

/// Write classic PCM WAV (WAVE_FORMAT_PCM tag=1, fmt chunk size=16) with 24-bit mono.
/// No LIST/INFO, no BEXT, no extensible.
fn write_classic_pcm24_wav(path: &Path, samples: &[i32]) -> Result<()> {
    let mut f = File::create(path).with_context(|| format!("create {:?}", path))?;

    // data chunk size in bytes: N * 3 bytes per 24-bit sample
    let data_bytes = (samples.len() * 3) as u32;

    // RIFF size = 4 ("WAVE") + (8+fmt) + (8+data)
    // fmt chunk: 8 + 16
    let riff_size = 4u32 + (8 + 16) + (8 + data_bytes);

    // ---- RIFF header ----
    f.write_all(b"RIFF")?;
    f.write_all(&(riff_size).to_le_bytes())?;
    f.write_all(b"WAVE")?;

    // ---- fmt chunk (classic PCM) ----
    f.write_all(b"fmt ")?;
    f.write_all(&(16u32).to_le_bytes())?; // fmt chunk size

    let audio_format: u16 = 1; // PCM
    let num_channels: u16 = 1;
    let sample_rate: u32 = TARGET_SR;
    let bits_per_sample: u16 = 24;

    let block_align: u16 = num_channels * (bits_per_sample / 8);
    let byte_rate: u32 = sample_rate * block_align as u32;

    f.write_all(&audio_format.to_le_bytes())?;
    f.write_all(&num_channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bits_per_sample.to_le_bytes())?;

    // ---- data chunk ----
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;

    // Write 24-bit little-endian for each i32 sample.
    // Clamp to signed 24-bit range to be safe: [-8388608, 8388607]
    for &s in samples {
        let mut v = s;
        if v > 8_388_607 { v = 8_388_607; }
        if v < -8_388_608 { v = -8_388_608; }

        // two's complement 24-bit little endian: lowest 3 bytes
        let u = (v as i32) as u32;
        let b0 = (u & 0xFF) as u8;
        let b1 = ((u >> 8) & 0xFF) as u8;
        let b2 = ((u >> 16) & 0xFF) as u8;
        f.write_all(&[b0, b1, b2])?;
    }

    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <input_24b_48k_mono.wav> <output.wav>", args[0]);
        std::process::exit(1);
    }

    let in_path = Path::new(&args[1]);
    let out_path = Path::new(&args[2]);

    let mut samples = read_pcm24_mono(in_path)?;

    // Make exactly 1024 samples: trim or pad with zeros.
    if samples.len() >= TARGET_SAMPLES {
        samples.truncate(TARGET_SAMPLES);
    } else {
        samples.resize(TARGET_SAMPLES, 0);
    }

    write_classic_pcm24_wav(out_path, &samples)?;

    println!("OK: wrote classic PCM WAV (fmt=16, tag=1), 24-bit/48k/mono, 1024 samples -> {:?}", out_path);
    Ok(())
}

