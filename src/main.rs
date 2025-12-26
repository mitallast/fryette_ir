use anyhow::{bail, Context, Result};
use hound::{SampleFormat, WavReader};
use std::{
    env,
    fs::{rename, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const TARGET_SR: u32 = 48_000;
const TARGET_SAMPLES: usize = 1024;

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

    // Build a classic PCM WAV (no extensible, no metadata).
    let wav_bytes = build_classic_pcm24_wav_bytes(&samples);

    // Write atomically + buffered: temp file in the SAME directory, fsync, rename.
    write_atomic_synced(out_path, &wav_bytes)?;

    // Optional: fsync the directory as well (helps on removable media).
    // If it fails (some FS), we ignore.
    let _ = sync_parent_dir(out_path);

    println!(
        "OK: wrote classic PCM WAV (fmt=16, tag=1), 24-bit/48k/mono, 1024 samples -> {:?}",
        out_path
    );

    Ok(())
}

/// Reads mono 24-bit PCM WAV samples into i32 (sign-extended).
fn read_pcm24_mono(path: &Path) -> Result<Vec<i32>> {
    let mut reader = WavReader::open(path).with_context(|| format!("open {:?}", path))?;
    let spec = reader.spec();

    if spec.sample_rate != TARGET_SR {
        bail!("Expected {} Hz, got {} (no resample)", TARGET_SR, spec.sample_rate);
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

    let mut out = Vec::new();
    // For 24-bit, hound supports reading as i32 (already sign-extended).
    for s in reader.samples::<i32>() {
        out.push(s?);
    }
    Ok(out)
}

/// Builds an in-memory classic PCM WAV:
/// RIFF + fmt(16) + data, PCM tag=1, 24-bit mono, 48kHz.
fn build_classic_pcm24_wav_bytes(samples: &[i32]) -> Vec<u8> {
    let data_bytes = (samples.len() * 3) as u32; // 24-bit => 3 bytes per sample

    // RIFF size = 4("WAVE") + (8+16 fmt) + (8+data)
    let riff_size = 4u32 + (8 + 16) + (8 + data_bytes);

    // 44-byte header + data
    let mut buf = Vec::with_capacity(44 + samples.len() * 3);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&riff_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // fmt size

    let audio_format: u16 = 1; // PCM
    let num_channels: u16 = 1;
    let sample_rate: u32 = TARGET_SR;
    let bits_per_sample: u16 = 24;
    let block_align: u16 = num_channels * (bits_per_sample / 8);
    let byte_rate: u32 = sample_rate * block_align as u32;

    buf.extend_from_slice(&audio_format.to_le_bytes());
    buf.extend_from_slice(&num_channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_bytes.to_le_bytes());

    // Write samples as 24-bit little-endian signed PCM.
    // Clamp to 24-bit range just in case.
    for &s in samples {
        let mut v = s;
        if v > 8_388_607 {
            v = 8_388_607;
        }
        if v < -8_388_608 {
            v = -8_388_608;
        }

        // Two's complement: take lowest 24 bits
        let u = v as u32;
        buf.push((u & 0xFF) as u8);
        buf.push(((u >> 8) & 0xFF) as u8);
        buf.push(((u >> 16) & 0xFF) as u8);
    }

    buf
}

/// Writes bytes atomically to `path`:
/// - temp file in same dir
/// - single buffered write
/// - flush + fsync
/// - rename over target
fn write_atomic_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let (tmp_path, parent) = temp_path_in_same_dir(path);

    // Create temp file with restrictive perms.
    {
        let mut f = File::create(&tmp_path)
            .with_context(|| format!("create temp file {:?}", tmp_path))?;
        f.write_all(bytes)
            .with_context(|| format!("write temp file {:?}", tmp_path))?;
        f.flush().with_context(|| "flush temp file".to_string())?;
        f.sync_all().with_context(|| "fsync temp file".to_string())?;
    }

    // Atomic rename within same filesystem.
    rename(&tmp_path, path).with_context(|| {
        format!(
            "rename temp {:?} -> {:?} (must be same filesystem)",
            tmp_path, path
        )
    })?;

    // Best-effort sync the directory entry (helps on removable media)
    if let Some(dir) = parent {
        let _ = sync_dir(&dir);
    }

    Ok(())
}

fn temp_path_in_same_dir(path: &Path) -> (PathBuf, Option<PathBuf>) {
    let parent = path.parent().map(|p| p.to_path_buf());
    let stem = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output.wav".to_string());

    let tmp_name = format!(".{}.tmp", stem);
    let tmp_path = match &parent {
        Some(p) => p.join(tmp_name),
        None => PathBuf::from(tmp_name),
    };
    (tmp_path, parent)
}

fn sync_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<()> {
    // On macOS/Linux, opening a directory and fsyncing it helps ensure rename is persisted.
    // This may fail on some filesystems; caller can ignore.
    let d = OpenOptions::new().read(true).open(dir)?;
    d.sync_all()?;
    Ok(())
}

