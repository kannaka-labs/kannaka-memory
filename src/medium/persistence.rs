//! File and git persistence for the Holographic Resonance Medium.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::Command;

use ndarray::{Array1, Array2};
use crate::codebook::Codebook;
use crate::consciousness::ConsciousnessState;

use super::Medium;
use super::types::*;

impl Medium {
    /// Save the medium to a .hrm file (atomic: writes to .tmp then renames).
    /// Note: only the `wavefront_count()` active rows are serialized, so excess
    /// in-RAM capacity from amortized growth never reaches disk. Calling
    /// `compact()` first is a memory optimization, not a correctness
    /// requirement. (`save_and_commit` takes `&self` and so cannot compact —
    /// #362.)
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), MediumError> {
        let path_ref = path.as_ref();
        // Per-writer-private temp file (pid + nanos), mirroring the chiral save
        // path. A FIXED shared `.hrm.tmp` let two concurrent writers truncate each
        // other mid-stream; blake3 was then hashed over the interleaved bytes and
        // the rename landed a checksum-VALID-but-corrupt .hrm (Oracle 2026-05-26).
        // The chiral path + the reactivation atomic_write were hardened; this flat
        // path (fresh-bootstrap first save + v1-fallback stores) was missed.
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_path = path_ref.with_extension(format!("hrm.tmp.{pid}.{nanos}"));
        // Same orphan handling as the chiral save: sweep dead writers' temps
        // first, and never leave our own behind on a failed save.
        crate::fs_util::sweep_stale_tmps(path_ref, crate::fs_util::STALE_TMP_AGE);
        let mut tmp_guard = crate::fs_util::TmpGuard::new(&tmp_path);
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);

        // Magic bytes
        writer.write_all(&HRM_MAGIC)?;

        // Version
        writer.write_all(&HRM_VERSION.to_le_bytes())?;

        // Timestamp
        let timestamp = chrono::Utc::now().timestamp_millis();
        writer.write_all(&timestamp.to_le_bytes())?;

        // Dimensions (N, D)
        let n = self.wavefront_count() as u32;
        let d = WAVEFRONT_DIM as u32;
        writer.write_all(&n.to_le_bytes())?;
        writer.write_all(&d.to_le_bytes())?;

        // Wavefronts tensor (row-major f32 array) — only active rows
        for i in 0..n as usize {
            for j in 0..WAVEFRONT_DIM {
                writer.write_all(&self.store.wavefronts[[i, j]].to_le_bytes())?;
            }
        }

        // Energy, frequency, phase arrays — only active entries
        for i in 0..n as usize {
            writer.write_all(&self.store.energy[i].to_le_bytes())?;
        }
        for i in 0..n as usize {
            writer.write_all(&self.store.frequency[i].to_le_bytes())?;
        }
        for i in 0..n as usize {
            writer.write_all(&self.store.phase[i].to_le_bytes())?;
        }

        // Timestamps — write exactly `n` entries to stay in lockstep with the
        // other per-active arrays. Pre-fix this wrote the full Vec, which broke
        // the loader whenever `timestamps.len()` drifted from active count.
        for i in 0..n as usize {
            let ts = self.store.timestamps.get(i).copied().unwrap_or(0);
            writer.write_all(&ts.to_le_bytes())?;
        }

        // Metadata (bincode serialized)
        let metadata_bytes = bincode::serialize(&self.store.metadata)?;
        let metadata_len = metadata_bytes.len() as u32;
        writer.write_all(&metadata_len.to_le_bytes())?;
        writer.write_all(&metadata_bytes)?;

        // Consciousness state (computed on-the-fly)
        let consciousness = self.compute_consciousness();
        let consciousness_bytes = bincode::serialize(&consciousness)?;
        let consciousness_len = consciousness_bytes.len() as u32;
        writer.write_all(&consciousness_len.to_le_bytes())?;
        writer.write_all(&consciousness_bytes)?;

        // Checksum (blake3 of all preceding data)
        // Flush data, compute checksum over the .tmp file, append checksum,
        // flush again — all BEFORE the atomic rename. No gap.
        writer.flush()?;
        drop(writer);

        // Compute checksum over all data written so far
        let mut file = File::open(&tmp_path)?;
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut file, &mut hasher)?;
        let checksum = hasher.finalize();
        drop(file);

        // Append checksum to the .tmp file and flush before rename
        let mut file = std::fs::OpenOptions::new().append(true).open(&tmp_path)?;
        file.write_all(checksum.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);

        // Atomic rename: tmp → final (data + checksum are both present, no gap)
        std::fs::rename(&tmp_path, path_ref)?;
        tmp_guard.disarm();

        Ok(())
    }

    /// Save the medium and commit to git with a message
    ///
    /// This function:
    /// 1. Saves the .hrm file (using existing save() method)
    /// 2. Runs `git add <filename>` on the saved file
    /// 3. Runs `git commit -m "<message>"`
    /// 4. Returns the commit hash
    /// 5. Optionally: `git push origin master` if push flag is set
    ///
    /// # Arguments
    /// * `path` - Path to save the .hrm file
    /// * `message` - Git commit message
    /// * `push` - Whether to push to origin master after committing
    pub fn save_and_commit<P: AsRef<Path>>(
        &self,
        path: P,
        message: &str,
        push: bool,
    ) -> Result<String, MediumError> {
        let path_ref = path.as_ref();

        // 1. Save the .hrm file (save uses wavefront_count() which returns self.len)
        self.save(path_ref)?;

        // 2. Git add the file
        let output = Command::new("git")
            .args(&["add", &path_ref.to_string_lossy()])
            .current_dir(path_ref.parent().unwrap_or(Path::new(".")))
            .output()
            .map_err(|e| MediumError::Git(format!("Failed to run git add: {e}")))?;

        if !output.status.success() {
            return Err(MediumError::Git(format!(
                "git add failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // 3. Git commit with message
        let output = Command::new("git")
            .args(&["commit", "-m", message])
            .current_dir(path_ref.parent().unwrap_or(Path::new(".")))
            .output()
            .map_err(|e| MediumError::Git(format!("Failed to run git commit: {e}")))?;

        if !output.status.success() {
            return Err(MediumError::Git(format!(
                "git commit failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // 4. Get the commit hash
        let output = Command::new("git")
            .args(&["rev-parse", "HEAD"])
            .current_dir(path_ref.parent().unwrap_or(Path::new(".")))
            .output()
            .map_err(|e| MediumError::Git(format!("Failed to get commit hash: {e}")))?;

        if !output.status.success() {
            return Err(MediumError::Git(format!(
                "git rev-parse failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let commit_hash = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // 5. Optional push to origin master
        if push {
            let output = Command::new("git")
                .args(&["push", "origin", "master"])
                .current_dir(path_ref.parent().unwrap_or(Path::new(".")))
                .output()
                .map_err(|e| MediumError::Git(format!("Failed to run git push: {e}")))?;

            if !output.status.success() {
                // Push failure is non-fatal, just log it
                eprintln!(
                    "Warning: git push failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }

        Ok(commit_hash)
    }

    /// Load a medium from git (historical version or working directory)
    ///
    /// # Arguments
    /// * `path` - Path to the .hrm file
    /// * `commit` - Optional commit hash. If None, loads from working directory.
    ///              If Some, runs `git show <commit>:<path>` to get historical version.
    pub fn load_from_git<P: AsRef<Path>>(
        path: P,
        commit: Option<&str>,
    ) -> Result<Medium, MediumError> {
        let path_ref = path.as_ref();

        match commit {
            None => {
                // Load from working directory (current file)
                Medium::load(path_ref)
            }
            Some(commit_hash) => {
                // Load historical version using git show
                let git_path = format!("{}:{}", commit_hash, path_ref.to_string_lossy());
                let output = Command::new("git")
                    .args(&["show", &git_path])
                    .current_dir(path_ref.parent().unwrap_or(Path::new(".")))
                    .output()
                    .map_err(|e| MediumError::Git(format!("Failed to run git show: {e}")))?;

                if !output.status.success() {
                    return Err(MediumError::Git(format!(
                        "git show failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }

                // Save git show output to a temporary file and load it
                use std::env;

                let temp_dir = env::temp_dir();
                let temp_path = temp_dir.join(format!("hrm_git_show_{commit_hash}"));

                {
                    let mut temp_file =
                        File::create(&temp_path).map_err(MediumError::Io)?;
                    temp_file
                        .write_all(&output.stdout)
                        .map_err(MediumError::Io)?;
                    temp_file.flush().map_err(MediumError::Io)?;
                }

                let result = Medium::load(&temp_path);

                // Clean up temp file
                let _ = std::fs::remove_file(&temp_path);

                result
            }
        }
    }

    /// Get git history for .hrm file
    ///
    /// Returns recent commits that touched the .hrm file, with metadata.
    ///
    /// # Arguments
    /// * `path` - Path to the .hrm file
    /// * `limit` - Maximum number of commits to return
    pub fn history<P: AsRef<Path>>(path: P, limit: usize) -> Result<Vec<HrmCommit>, MediumError> {
        let path_ref = path.as_ref();

        // Get git log for this specific file
        let output = Command::new("git")
            .args(&[
                "log",
                "--format=%H|%s|%ct", // hash|subject|timestamp
                &format!("-{limit}"),
                "--",
                &path_ref.to_string_lossy(),
            ])
            .current_dir(path_ref.parent().unwrap_or(Path::new(".")))
            .output()
            .map_err(|e| MediumError::Git(format!("Failed to run git log: {e}")))?;

        if !output.status.success() {
            return Err(MediumError::Git(format!(
                "git log failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let log_output = String::from_utf8_lossy(&output.stdout);
        let mut commits = Vec::new();

        for line in log_output.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 3 {
                let hash = parts[0].to_string();
                let message = parts[1].to_string();
                let timestamp = parts[2].parse::<i64>().unwrap_or(0);

                // Try to extract wavefront count from the commit
                let wavefront_count = extract_wavefront_count(&message);

                commits.push(HrmCommit {
                    hash,
                    message,
                    timestamp,
                    wavefront_count,
                });
            }
        }

        Ok(commits)
    }

    /// Load a medium from a .hrm file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, MediumError> {
        let path = path.as_ref();

        // File size sanity check: minimum = magic(4) + version(4) + timestamp(8) + n(4) + d(4) = 24 bytes
        const MIN_HRM_SIZE: u64 = 24;
        let file_size = std::fs::metadata(path)?.len();
        if file_size < MIN_HRM_SIZE {
            return Err(MediumError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "HRM file too small ({file_size} bytes, minimum {MIN_HRM_SIZE}): likely truncated"
                ),
            )));
        }

        // Verify trailing blake3 checksum before parsing (shared with v2).
        super::chiral_persistence::verify_blake3_trailing(path)?;

        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        // Magic bytes
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if magic != HRM_MAGIC {
            return Err(MediumError::InvalidMagic);
        }

        // Version
        let mut version_bytes = [0u8; 4];
        reader.read_exact(&mut version_bytes)?;
        let version = u32::from_le_bytes(version_bytes);
        if version != HRM_VERSION {
            return Err(MediumError::UnsupportedVersion(version));
        }

        // Timestamp (for info, not used in loading)
        let mut ts_bytes = [0u8; 8];
        reader.read_exact(&mut ts_bytes)?;

        // Dimensions
        let mut n_bytes = [0u8; 4];
        let mut d_bytes = [0u8; 4];
        reader.read_exact(&mut n_bytes)?;
        reader.read_exact(&mut d_bytes)?;
        let n = u32::from_le_bytes(n_bytes) as usize;
        let d = u32::from_le_bytes(d_bytes) as usize;

        if d != WAVEFRONT_DIM {
            return Err(MediumError::DimensionMismatch {
                expected: WAVEFRONT_DIM,
                actual: d,
            });
        }

        // #367: `n` is read straight from the file and sizes the tensor before
        // any body bytes are read. Cap it so a corrupt count can't request a
        // multi-TB allocation and abort the process before the checksum runs.
        // (`d` is already validated to WAVEFRONT_DIM above.)
        const MAX_WAVEFRONT_ELEMS: u64 = 1024 * 1024 * 1024; // 4 GiB of f32
        match (n as u64).checked_mul(d as u64) {
            Some(e) if e <= MAX_WAVEFRONT_ELEMS => {}
            _ => {
                return Err(MediumError::CorruptHrm(format!(
                    "implausible wavefront tensor n={n} d={d} (max {MAX_WAVEFRONT_ELEMS} elems) — v1 layout desync"
                )));
            }
        }

        // Wavefronts tensor
        let mut wavefront_data = vec![0.0f32; n * d];
        for val in &mut wavefront_data {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }
        let wavefronts = Array2::from_shape_vec((n, d), wavefront_data).unwrap();

        // Energy, frequency, phase arrays
        let mut energy_data = vec![0.0f32; n];
        let mut frequency_data = vec![0.0f32; n];
        let mut phase_data = vec![0.0f32; n];

        for val in &mut energy_data {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }
        for val in &mut frequency_data {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }
        for val in &mut phase_data {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }

        let energy = Array1::from_vec(energy_data);
        let frequency = Array1::from_vec(frequency_data);
        let phase = Array1::from_vec(phase_data);

        // Timestamps
        let mut timestamps = vec![0i64; n];
        for ts in &mut timestamps {
            let mut bytes = [0u8; 8];
            reader.read_exact(&mut bytes)?;
            *ts = i64::from_le_bytes(bytes);
        }

        // Metadata
        let mut len_bytes = [0u8; 4];
        reader.read_exact(&mut len_bytes)?;
        let metadata_len = u32::from_le_bytes(len_bytes) as usize;
        const MAX_META_BYTES: usize = 256 * 1024 * 1024;
        if metadata_len > MAX_META_BYTES {
            return Err(MediumError::CorruptHrm(format!(
                "implausible metadata_len={metadata_len} (max {MAX_META_BYTES}) — v1 medium layout desync"
            )));
        }
        let mut metadata_bytes = vec![0u8; metadata_len];
        reader.read_exact(&mut metadata_bytes)?;
        // Fall back through the legacy layouts for flat v1 files written before
        // newer fields were appended (mirrors the chiral read path):
        // new → pre-temporal (Task 3.2b) → pre-tier (ADR-0031).
        let metadata: Vec<WavefrontMeta> =
            super::chiral_persistence::decode_wavefront_metadata(&metadata_bytes)?;

        // Consciousness state (for info, not essential for loading)
        let mut len_bytes = [0u8; 4];
        reader.read_exact(&mut len_bytes)?;
        let consciousness_len = u32::from_le_bytes(len_bytes) as usize;
        // #367: cap the untrusted section length before allocating.
        if consciousness_len > MAX_META_BYTES {
            return Err(MediumError::CorruptHrm(format!(
                "implausible consciousness_len={consciousness_len} (max {MAX_META_BYTES}) — v1 layout desync"
            )));
        }
        let mut consciousness_bytes = vec![0u8; consciousness_len];
        reader.read_exact(&mut consciousness_bytes)?;
        let _consciousness: ConsciousnessState = bincode::deserialize(&consciousness_bytes)?;

        // Verify checksum (blake3 of all data except the final 32 bytes)
        // Use read() instead of read_exact() — a truncated/missing checksum
        // should warn, not hard-fail with "failed to fill whole buffer".
        let mut stored_checksum = [0u8; 32];
        let mut checksum_bytes_read = 0;
        loop {
            match reader.read(&mut stored_checksum[checksum_bytes_read..]) {
                Ok(0) => break,
                Ok(n) => {
                    checksum_bytes_read += n;
                    if checksum_bytes_read >= 32 {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if checksum_bytes_read == 32 {
            // Re-read file and hash everything except the final 32 bytes
            let file_len = std::fs::metadata(path)?.len();
            if file_len >= 32 {
                let data_len = file_len - 32;
                let mut file = File::open(path)?;
                let mut hasher = blake3::Hasher::new();
                let mut remaining = data_len;
                let mut buf = [0u8; 8192];
                while remaining > 0 {
                    let to_read = (remaining as usize).min(buf.len());
                    file.read_exact(&mut buf[..to_read])?;
                    hasher.update(&buf[..to_read]);
                    remaining -= to_read as u64;
                }
                let computed = hasher.finalize();
                if computed.as_bytes() != &stored_checksum {
                    return Err(MediumError::ChecksumMismatch);
                }
            }
        } else if checksum_bytes_read > 0 {
            // Partial checksum — file was likely truncated between data and checksum write.
            // Warn but don't fail: the data portion parsed successfully.
            eprintln!(
                "Warning: HRM file has partial checksum ({checksum_bytes_read}/32 bytes) — skipping verification. \
                 File may have been saved by an older version or interrupted during write."
            );
        }
        // If no checksum present (old format / 0 bytes read), skip verification gracefully

        // #362: same independent-section-size guard as the chiral read path.
        // energy/frequency/phase/wavefronts/timestamps are sized by `n`; the
        // metadata Vec is sized by its own bincode length. A mismatch would let
        // index-by-`n` accesses run off the metadata Vec — surface it as
        // CorruptHrm rather than trusting one section.
        if metadata.len() != n || timestamps.len() != n {
            return Err(MediumError::CorruptHrm(format!(
                "v1 medium section-size desync: count={} metadata={} timestamps={}",
                n, metadata.len(), timestamps.len()
            )));
        }

        // Build ID -> index mapping
        let mut id_to_index = HashMap::new();
        for (i, meta) in metadata.iter().enumerate() {
            id_to_index.insert(meta.id, i);
        }

        Ok(Self {
            store: super::WavefrontStore {
                len: n,
                dims: WAVEFRONT_DIM,
                wavefronts,
                energy,
                frequency,
                phase,
                timestamps,
                metadata,
                id_to_index,
            },
            audio_codebook: Codebook::new(AUDIO_FEATURE_DIM, WAVEFRONT_DIM, AUDIO_CODEBOOK_SEED),
            visual_codebook: Codebook::new(
                VISUAL_FEATURE_DIM,
                WAVEFRONT_DIM,
                VISUAL_CODEBOOK_SEED,
            ),
            total_energy_added: 0.0,
            total_energy_dampened: 0.0,
        })
    }
}
