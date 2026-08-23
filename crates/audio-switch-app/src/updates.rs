use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use audio_switch_core::UpdatePolicy;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const RELEASE_API: &str = "https://api.github.com/repos/2sBs/audio-switch/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/2sBs/audio-switch/releases/download/";
const USER_AGENT: &str = concat!("Audio-Switch/", env!("CARGO_PKG_VERSION"));
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_INSTALLER_BYTES: u64 = 64 * 1024 * 1024;
const INITIAL_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const STATE_SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    Available(ReleaseInfo),
    Downloading { release: ReleaseInfo, percent: u8 },
    Ready(ReleaseInfo),
    Installing(ReleaseInfo),
    UpToDate,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSnapshot {
    pub current_version: String,
    pub installed: bool,
    pub last_checked_unix: Option<u64>,
    pub status: UpdateStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    CheckNow,
    Download,
}

pub enum StartOutcome {
    Installing,
    Ready(UpdateController),
}

#[derive(Clone)]
pub struct UpdateController {
    commands: Sender<WorkerCommand>,
    shared: Arc<Mutex<SharedState>>,
    launcher: Arc<dyn InstallerLauncher>,
    paths: UpdatePaths,
}

impl UpdateController {
    pub fn start(policy: UpdatePolicy, config_path: &Path) -> Result<StartOutcome> {
        let paths = UpdatePaths::for_config(config_path)?;
        let current_version = Version::parse(env!("CARGO_PKG_VERSION"))
            .context("parsing the installed Audio Switch version")?;
        let installed = installed_copy().unwrap_or_else(|error| {
            eprintln!("Audio Switch could not identify the installer build: {error:#}");
            false
        });
        start_with(
            policy,
            current_version,
            installed,
            paths,
            Arc::new(GithubReleaseSource::new()),
            Arc::new(ProcessInstallerLauncher),
            Schedule::default(),
        )
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.shared
            .lock()
            .expect("update state mutex poisoned")
            .snapshot
            .clone()
    }

    pub fn request(&self, action: UpdateAction) -> Result<()> {
        let command = match action {
            UpdateAction::CheckNow => WorkerCommand::CheckNow,
            UpdateAction::Download => WorkerCommand::Download,
        };
        self.commands
            .send(command)
            .context("the update worker is unavailable")
    }

    pub fn set_policy(&self, policy: UpdatePolicy) -> Result<()> {
        self.commands
            .send(WorkerCommand::SetPolicy(policy))
            .context("the update worker is unavailable")
    }

    pub fn install_and_restart(&self) -> Result<()> {
        launch_pending(&self.shared, &self.paths, self.launcher.as_ref())
    }

    pub fn stop(&self) {
        let _ = self.commands.send(WorkerCommand::Stop);
    }
}

#[derive(Clone)]
struct UpdatePaths {
    root: PathBuf,
    state: PathBuf,
}

impl UpdatePaths {
    fn for_config(config_path: &Path) -> Result<Self> {
        let config_root = config_path
            .parent()
            .context("the Audio Switch configuration path has no parent")?;
        Ok(Self::at(config_root.join("updates")))
    }

    fn at(root: PathBuf) -> Self {
        Self {
            state: root.join("state.json"),
            root,
        }
    }

    fn installer(&self, file_name: &str) -> Result<PathBuf> {
        if Path::new(file_name)
            .file_name()
            .and_then(|value| value.to_str())
            != Some(file_name)
        {
            bail!("the pending update filename is invalid");
        }
        Ok(self.root.join(file_name))
    }
}

#[derive(Clone, Debug)]
struct ReleaseAsset {
    info: ReleaseInfo,
    version: Version,
    file_name: String,
    download_url: String,
    size: u64,
    sha256: String,
}

impl ReleaseAsset {
    fn expected_file_name(version: &Version) -> String {
        format!("audio-switch-{version}-setup-windows-x86_64.exe")
    }
}

#[derive(Debug)]
struct SharedState {
    snapshot: UpdateSnapshot,
    candidate: Option<ReleaseAsset>,
    persisted: PersistedState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingUpdate {
    version: String,
    file_name: String,
    notes_url: String,
    size: u64,
    sha256: String,
}

impl PendingUpdate {
    fn release_info(&self) -> ReleaseInfo {
        ReleaseInfo {
            version: self.version.clone(),
            notes_url: self.notes_url.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    schema: u32,
    last_checked_unix: Option<u64>,
    pending: Option<PendingUpdate>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            schema: STATE_SCHEMA,
            last_checked_unix: None,
            pending: None,
        }
    }
}

#[derive(Clone, Copy)]
struct Schedule {
    initial_delay: Duration,
    interval: Duration,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            initial_delay: INITIAL_CHECK_DELAY,
            interval: CHECK_INTERVAL,
        }
    }
}

enum WorkerCommand {
    CheckNow,
    Download,
    SetPolicy(UpdatePolicy),
    Stop,
}

trait ReleaseSource: Send + Sync {
    fn latest(&self, current_version: &Version) -> Result<Option<ReleaseAsset>>;
    fn download(
        &self,
        release: &ReleaseAsset,
        destination: &Path,
        progress: &mut dyn FnMut(u8),
    ) -> Result<()>;
}

trait InstallerLauncher: Send + Sync {
    fn launch(&self, installer: &Path) -> Result<()>;
}

struct GithubReleaseSource {
    agent: ureq::Agent,
}

impl GithubReleaseSource {
    fn new() -> Self {
        Self {
            agent: ureq::Agent::new_with_defaults(),
        }
    }
}

impl ReleaseSource for GithubReleaseSource {
    fn latest(&self, current_version: &Version) -> Result<Option<ReleaseAsset>> {
        let mut response = self
            .agent
            .get(RELEASE_API)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .call()
            .context("requesting the latest GitHub release")?;
        let release: GithubRelease = response
            .body_mut()
            .with_config()
            .limit(MAX_METADATA_BYTES)
            .read_json()
            .context("reading the latest GitHub release")?;
        select_release(release, current_version)
    }

    fn download(
        &self,
        release: &ReleaseAsset,
        destination: &Path,
        progress: &mut dyn FnMut(u8),
    ) -> Result<()> {
        let mut response = self
            .agent
            .get(release.download_url.as_str())
            .header("Accept", "application/octet-stream")
            .header("User-Agent", USER_AGENT)
            .call()
            .context("downloading the Audio Switch installer")?;
        let reader = response
            .body_mut()
            .with_config()
            .limit(release.size.saturating_add(1))
            .reader();
        write_verified_installer(reader, release, destination, progress)
    }
}

struct ProcessInstallerLauncher;

impl InstallerLauncher for ProcessInstallerLauncher {
    fn launch(&self, installer: &Path) -> Result<()> {
        ProcessCommand::new(installer)
            .args([
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/CLOSEAPPLICATIONS",
                "/UPDATE=1",
            ])
            .spawn()
            .with_context(|| format!("launching update installer {}", installer.display()))?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

fn select_release(release: GithubRelease, current: &Version) -> Result<Option<ReleaseAsset>> {
    if release.draft || release.prerelease {
        bail!("GitHub returned a draft or prerelease from the stable release endpoint");
    }
    let version_text = release
        .tag_name
        .strip_prefix('v')
        .context("the latest release tag does not start with 'v'")?;
    let version = Version::parse(version_text).context("parsing the latest release version")?;
    if !version.pre.is_empty() {
        bail!("the latest stable release has a prerelease version");
    }
    if version <= *current {
        return Ok(None);
    }

    let expected_name = ReleaseAsset::expected_file_name(&version);
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == expected_name)
        .with_context(|| format!("release v{version} has no {expected_name} asset"))?;
    if asset.size == 0 || asset.size > MAX_INSTALLER_BYTES {
        bail!("the installer has an invalid size of {} bytes", asset.size);
    }
    let digest = asset.digest.context("the installer has no GitHub digest")?;
    let sha256 = digest
        .strip_prefix("sha256:")
        .context("the installer digest is not SHA-256")?
        .to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|value| value.is_ascii_hexdigit()) {
        bail!("the installer SHA-256 digest is invalid");
    }
    if !asset
        .browser_download_url
        .starts_with(RELEASE_DOWNLOAD_PREFIX)
    {
        bail!("the installer download URL is not an Audio Switch GitHub release");
    }
    let expected_notes_url =
        format!("https://github.com/2sBs/audio-switch/releases/tag/v{version}");
    if release.html_url != expected_notes_url {
        bail!("the release notes URL is not an Audio Switch GitHub release");
    }

    Ok(Some(ReleaseAsset {
        info: ReleaseInfo {
            version: version.to_string(),
            notes_url: release.html_url,
        },
        version,
        file_name: expected_name,
        download_url: asset.browser_download_url,
        size: asset.size,
        sha256,
    }))
}

fn write_verified_installer(
    mut reader: impl Read,
    release: &ReleaseAsset,
    destination: &Path,
    progress: &mut dyn FnMut(u8),
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).context("creating the update download directory")?;
    }
    let mut file = File::create(destination).context("creating the partial update installer")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut downloaded = 0_u64;
    let mut last_percent = 0_u8;
    loop {
        let count = reader
            .read(&mut buffer)
            .context("reading the update download")?;
        if count == 0 {
            break;
        }
        downloaded = downloaded
            .checked_add(count as u64)
            .context("the update size overflowed")?;
        if downloaded > release.size {
            bail!("the installer download exceeded its declared size");
        }
        file.write_all(&buffer[..count])
            .context("writing the update download")?;
        hasher.update(&buffer[..count]);
        let percent = ((downloaded.saturating_mul(100)) / release.size).min(100) as u8;
        if percent != last_percent {
            last_percent = percent;
            progress(percent);
        }
    }
    file.sync_all().context("flushing the update installer")?;
    if downloaded != release.size {
        bail!(
            "the installer download was {downloaded} bytes; expected {}",
            release.size
        );
    }
    let digest = hasher.finalize();
    let actual = format_digest(&digest);
    if actual != release.sha256 {
        bail!("the installer SHA-256 digest did not match GitHub");
    }
    progress(100);
    Ok(())
}

fn format_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

fn start_with(
    policy: UpdatePolicy,
    current_version: Version,
    installed: bool,
    paths: UpdatePaths,
    source: Arc<dyn ReleaseSource>,
    launcher: Arc<dyn InstallerLauncher>,
    schedule: Schedule,
) -> Result<StartOutcome> {
    let mut startup_failure = None;
    let mut persisted = load_state(&paths.state).unwrap_or_else(|error| {
        startup_failure = Some(format!("The saved update state was ignored: {error:#}"));
        PersistedState::default()
    });
    if let Err(error) = fs::create_dir_all(&paths.root)
        .context("creating the Audio Switch update directory")
        .and_then(|()| reconcile_pending(&paths, &current_version, &mut persisted))
        .and_then(|()| save_state(&paths.state, &persisted))
    {
        startup_failure = Some(format!("The updater could not initialize: {error:#}"));
        persisted.pending = None;
    }

    let pending = persisted.pending.clone();
    let initial_status = startup_failure
        .map(UpdateStatus::Failed)
        .unwrap_or_else(|| {
            pending
                .as_ref()
                .map(|pending| UpdateStatus::Ready(pending.release_info()))
                .unwrap_or(UpdateStatus::Idle)
        });
    let shared = Arc::new(Mutex::new(SharedState {
        snapshot: UpdateSnapshot {
            current_version: current_version.to_string(),
            installed,
            last_checked_unix: persisted.last_checked_unix,
            status: initial_status,
        },
        candidate: None,
        persisted,
    }));

    if policy == UpdatePolicy::Automatic && installed && pending.is_some() {
        match launch_pending(&shared, &paths, launcher.as_ref()) {
            Ok(()) => return Ok(StartOutcome::Installing),
            Err(error) => set_status(
                &shared,
                UpdateStatus::Failed(format!("The update could not start: {error:#}")),
            ),
        }
    }

    let (commands, receiver) = mpsc::channel();
    let worker_shared = Arc::clone(&shared);
    let worker_paths = paths.clone();
    thread::Builder::new()
        .name("audio-switch-updates".into())
        .spawn(move || {
            worker_loop(
                receiver,
                worker_shared,
                worker_paths,
                source,
                current_version,
                policy,
                installed,
                schedule,
            )
        })
        .context("starting the update worker")?;

    Ok(StartOutcome::Ready(UpdateController {
        commands,
        shared,
        launcher,
        paths,
    }))
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    receiver: Receiver<WorkerCommand>,
    shared: Arc<Mutex<SharedState>>,
    paths: UpdatePaths,
    source: Arc<dyn ReleaseSource>,
    current_version: Version,
    mut policy: UpdatePolicy,
    installed: bool,
    schedule: Schedule,
) {
    let mut next_check = next_check_deadline(&shared, policy, schedule);
    loop {
        let command = match next_check {
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(wait) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => WorkerCommand::CheckNow,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            None => match receiver.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };
        match command {
            WorkerCommand::CheckNow => {
                check_for_update(
                    &shared,
                    &paths,
                    source.as_ref(),
                    &current_version,
                    policy,
                    installed,
                );
                next_check = if policy == UpdatePolicy::Disabled {
                    None
                } else {
                    Some(Instant::now() + schedule.interval)
                };
            }
            WorkerCommand::Download => {
                download_candidate(&shared, &paths, source.as_ref(), installed);
            }
            WorkerCommand::SetPolicy(updated) => {
                policy = updated;
                if policy == UpdatePolicy::Automatic {
                    let has_candidate = shared
                        .lock()
                        .expect("update state mutex poisoned")
                        .candidate
                        .is_some();
                    if has_candidate && installed {
                        download_candidate(&shared, &paths, source.as_ref(), installed);
                    }
                    next_check = Some(Instant::now() + schedule.initial_delay);
                } else if policy == UpdatePolicy::Disabled {
                    next_check = None;
                } else if next_check.is_none() {
                    next_check = Some(Instant::now() + schedule.initial_delay);
                }
            }
            WorkerCommand::Stop => return,
        }
    }
}

fn next_check_deadline(
    shared: &Arc<Mutex<SharedState>>,
    policy: UpdatePolicy,
    schedule: Schedule,
) -> Option<Instant> {
    if policy == UpdatePolicy::Disabled {
        return None;
    }
    let last_checked = shared
        .lock()
        .expect("update state mutex poisoned")
        .persisted
        .last_checked_unix;
    let elapsed = last_checked
        .and_then(|checked| now_unix().checked_sub(checked))
        .map(Duration::from_secs);
    Some(match elapsed {
        Some(elapsed) if elapsed < schedule.interval => {
            Instant::now() + (schedule.interval - elapsed)
        }
        _ => Instant::now() + schedule.initial_delay,
    })
}

fn check_for_update(
    shared: &Arc<Mutex<SharedState>>,
    paths: &UpdatePaths,
    source: &dyn ReleaseSource,
    current_version: &Version,
    policy: UpdatePolicy,
    installed: bool,
) {
    set_status(shared, UpdateStatus::Checking);
    let checked_at = now_unix();
    let result = source.latest(current_version);
    {
        let mut state = shared.lock().expect("update state mutex poisoned");
        state.persisted.last_checked_unix = Some(checked_at);
        state.snapshot.last_checked_unix = Some(checked_at);
    }
    match result {
        Ok(Some(release)) => {
            let ready = {
                let state = shared.lock().expect("update state mutex poisoned");
                state
                    .persisted
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.version == release.version.to_string())
            };
            {
                let mut state = shared.lock().expect("update state mutex poisoned");
                state.candidate = Some(release.clone());
                state.snapshot.status = if ready {
                    UpdateStatus::Ready(release.info.clone())
                } else {
                    UpdateStatus::Available(release.info.clone())
                };
            }
            persist_shared(shared, paths);
            if policy == UpdatePolicy::Automatic && installed && !ready {
                download_candidate(shared, paths, source, installed);
            }
        }
        Ok(None) => {
            let mut state = shared.lock().expect("update state mutex poisoned");
            state.candidate = None;
            state.snapshot.status = state
                .persisted
                .pending
                .as_ref()
                .map(|pending| UpdateStatus::Ready(pending.release_info()))
                .unwrap_or(UpdateStatus::UpToDate);
            drop(state);
            persist_shared(shared, paths);
        }
        Err(error) => {
            set_status(shared, UpdateStatus::Failed(format!("{error:#}")));
            persist_shared(shared, paths);
        }
    }
}

fn download_candidate(
    shared: &Arc<Mutex<SharedState>>,
    paths: &UpdatePaths,
    source: &dyn ReleaseSource,
    installed: bool,
) {
    let Some(release) = shared
        .lock()
        .expect("update state mutex poisoned")
        .candidate
        .clone()
    else {
        return;
    };
    if !installed {
        set_status(
            shared,
            UpdateStatus::Failed(
                "Automatic installation requires the Audio Switch installer build.".into(),
            ),
        );
        return;
    }

    let final_path = match paths.installer(&release.file_name) {
        Ok(path) => path,
        Err(error) => {
            set_status(shared, UpdateStatus::Failed(format!("{error:#}")));
            return;
        }
    };
    if verify_file(&final_path, release.size, &release.sha256).is_ok() {
        finish_download(shared, paths, &release);
        return;
    }
    let partial_path = final_path.with_extension("exe.partial");
    set_status(
        shared,
        UpdateStatus::Downloading {
            release: release.info.clone(),
            percent: 0,
        },
    );
    let result = source.download(&release, &partial_path, &mut |percent| {
        set_status(
            shared,
            UpdateStatus::Downloading {
                release: release.info.clone(),
                percent,
            },
        );
    });
    if let Err(error) = result {
        let _ = fs::remove_file(&partial_path);
        set_status(shared, UpdateStatus::Failed(format!("{error:#}")));
        return;
    }
    if final_path.exists()
        && let Err(error) = fs::remove_file(&final_path)
    {
        set_status(shared, UpdateStatus::Failed(error.to_string()));
        return;
    }
    if let Err(error) = fs::rename(&partial_path, &final_path) {
        set_status(shared, UpdateStatus::Failed(error.to_string()));
        return;
    }
    finish_download(shared, paths, &release);
}

fn finish_download(shared: &Arc<Mutex<SharedState>>, paths: &UpdatePaths, release: &ReleaseAsset) {
    {
        let mut state = shared.lock().expect("update state mutex poisoned");
        state.persisted.pending = Some(PendingUpdate {
            version: release.version.to_string(),
            file_name: release.file_name.clone(),
            notes_url: release.info.notes_url.clone(),
            size: release.size,
            sha256: release.sha256.clone(),
        });
        state.snapshot.status = UpdateStatus::Ready(release.info.clone());
    }
    persist_shared(shared, paths);
}

fn launch_pending(
    shared: &Arc<Mutex<SharedState>>,
    paths: &UpdatePaths,
    launcher: &dyn InstallerLauncher,
) -> Result<()> {
    let pending = shared
        .lock()
        .expect("update state mutex poisoned")
        .persisted
        .pending
        .clone()
        .context("no verified update is ready to install")?;
    let installer = paths.installer(&pending.file_name)?;
    verify_file(&installer, pending.size, &pending.sha256)
        .context("rechecking the pending update installer")?;

    {
        let mut state = shared.lock().expect("update state mutex poisoned");
        state.persisted.pending = None;
        save_state(&paths.state, &state.persisted)?;
    }
    if let Err(error) = launcher.launch(&installer) {
        let mut state = shared.lock().expect("update state mutex poisoned");
        state.persisted.pending = Some(pending.clone());
        save_state(&paths.state, &state.persisted)?;
        return Err(error);
    }
    set_status(shared, UpdateStatus::Installing(pending.release_info()));
    Ok(())
}

fn reconcile_pending(
    paths: &UpdatePaths,
    current_version: &Version,
    state: &mut PersistedState,
) -> Result<()> {
    let Some(pending) = state.pending.clone() else {
        return Ok(());
    };
    let version = Version::parse(&pending.version).context("parsing the pending update version")?;
    let installer = paths.installer(&pending.file_name)?;
    if version <= *current_version {
        let _ = fs::remove_file(installer);
        state.pending = None;
        return Ok(());
    }
    if verify_file(&installer, pending.size, &pending.sha256).is_err() {
        let _ = fs::remove_file(installer);
        state.pending = None;
    }
    Ok(())
}

fn verify_file(path: &Path, expected_size: u64, expected_sha256: &str) -> Result<()> {
    let mut file = File::open(path).context("opening the update installer")?;
    if file.metadata()?.len() != expected_size {
        bail!("the update installer size changed");
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let digest = hasher.finalize();
    if format_digest(&digest) != expected_sha256 {
        bail!("the update installer digest changed");
    }
    Ok(())
}

fn installed_copy() -> Result<bool> {
    let executable = std::env::current_exe().context("locating the Audio Switch executable")?;
    let directory = executable
        .parent()
        .context("the Audio Switch executable has no parent directory")?;
    for entry in fs::read_dir(directory).context("checking the Audio Switch install directory")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("unins") && name.ends_with(".exe") && entry.file_type()?.is_file() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn load_state(path: &Path) -> Result<PersistedState> {
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let state: PersistedState =
        serde_json::from_slice(&fs::read(path).context("reading update state")?)
            .context("parsing update state")?;
    if state.schema != STATE_SCHEMA {
        bail!("unsupported update-state schema {}", state.schema);
    }
    Ok(state)
}

fn save_state(path: &Path, state: &PersistedState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("creating the update-state directory")?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary).context("creating temporary update state")?;
    file.write_all(&serde_json::to_vec_pretty(state)?)
        .context("writing update state")?;
    file.sync_all().context("flushing update state")?;
    if path.exists() {
        fs::remove_file(path).context("replacing old update state")?;
    }
    fs::rename(temporary, path).context("installing new update state")
}

fn persist_shared(shared: &Arc<Mutex<SharedState>>, paths: &UpdatePaths) {
    let state = shared.lock().expect("update state mutex poisoned");
    if let Err(error) = save_state(&paths.state, &state.persisted) {
        eprintln!("Audio Switch could not save update state: {error:#}");
    }
}

fn set_status(shared: &Arc<Mutex<SharedState>>, status: UpdateStatus) {
    shared
        .lock()
        .expect("update state mutex poisoned")
        .snapshot
        .status = status;
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn release_json(version: &str, bytes: &[u8]) -> GithubRelease {
        let digest = Sha256::digest(bytes);
        let digest = format_digest(&digest);
        GithubRelease {
            tag_name: format!("v{version}"),
            html_url: format!("https://github.com/2sBs/audio-switch/releases/tag/v{version}"),
            draft: false,
            prerelease: false,
            assets: vec![GithubAsset {
                name: format!("audio-switch-{version}-setup-windows-x86_64.exe"),
                browser_download_url: format!(
                    "https://github.com/2sBs/audio-switch/releases/download/v{version}/audio-switch-{version}-setup-windows-x86_64.exe"
                ),
                size: bytes.len() as u64,
                digest: Some(format!("sha256:{digest}")),
            }],
        }
    }

    #[test]
    fn selects_the_exact_installer_from_a_newer_stable_release() {
        let release = select_release(
            release_json("0.5.0", b"installer"),
            &Version::parse("0.4.0").unwrap(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(release.version, Version::parse("0.5.0").unwrap());
        assert_eq!(
            release.file_name,
            "audio-switch-0.5.0-setup-windows-x86_64.exe"
        );
    }

    #[test]
    fn ignores_a_release_that_is_not_newer() {
        let release = select_release(
            release_json("0.4.0", b"installer"),
            &Version::parse("0.4.0").unwrap(),
        )
        .unwrap();

        assert!(release.is_none());
    }

    #[test]
    fn rejects_an_installer_from_an_unexpected_url() {
        let mut release = release_json("0.5.0", b"installer");
        release.assets[0].browser_download_url = "https://example.com/update.exe".into();

        let error = select_release(release, &Version::parse("0.4.0").unwrap()).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("not an Audio Switch GitHub release")
        );
    }

    #[test]
    fn rejects_release_notes_from_an_unexpected_url() {
        let mut release = release_json("0.5.0", b"installer");
        release.html_url = "https://example.com/release".into();

        let error = select_release(release, &Version::parse("0.4.0").unwrap()).unwrap_err();

        assert!(error.to_string().contains("release notes URL"));
    }

    #[test]
    fn verified_download_rejects_digest_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let mut release = select_release(
            release_json("0.5.0", b"expected"),
            &Version::parse("0.4.0").unwrap(),
        )
        .unwrap()
        .unwrap();
        release.size = 8;
        let destination = directory.path().join("update.partial");

        let error = write_verified_installer(
            Cursor::new(b"modified"),
            &release,
            &destination,
            &mut |_| {},
        )
        .unwrap_err();

        assert!(error.to_string().contains("did not match GitHub"));
    }

    struct FakeSource {
        release: ReleaseAsset,
        bytes: Vec<u8>,
    }

    struct CountingSource {
        release: ReleaseAsset,
        calls: Arc<AtomicUsize>,
    }

    impl ReleaseSource for CountingSource {
        fn latest(&self, _current_version: &Version) -> Result<Option<ReleaseAsset>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(self.release.clone()))
        }

        fn download(
            &self,
            _release: &ReleaseAsset,
            _destination: &Path,
            _progress: &mut dyn FnMut(u8),
        ) -> Result<()> {
            bail!("the test did not expect a download")
        }
    }

    impl ReleaseSource for FakeSource {
        fn latest(&self, _current_version: &Version) -> Result<Option<ReleaseAsset>> {
            Ok(Some(self.release.clone()))
        }

        fn download(
            &self,
            release: &ReleaseAsset,
            destination: &Path,
            progress: &mut dyn FnMut(u8),
        ) -> Result<()> {
            write_verified_installer(Cursor::new(&self.bytes), release, destination, progress)
        }
    }

    #[derive(Default)]
    struct FakeLauncher {
        launches: AtomicUsize,
    }

    impl InstallerLauncher for FakeLauncher {
        fn launch(&self, _installer: &Path) -> Result<()> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FailingLauncher;

    impl InstallerLauncher for FailingLauncher {
        fn launch(&self, _installer: &Path) -> Result<()> {
            bail!("launcher unavailable")
        }
    }

    fn wait_for_status(controller: &UpdateController, expected: fn(&UpdateStatus) -> bool) {
        for _ in 0..200 {
            if expected(&controller.snapshot().status) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("update worker did not reach the expected state");
    }

    #[test]
    fn automatic_check_downloads_and_persists_a_verified_installer() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = b"verified installer".to_vec();
        let release = select_release(
            release_json("0.5.0", &bytes),
            &Version::parse("0.4.0").unwrap(),
        )
        .unwrap()
        .unwrap();
        let source = Arc::new(FakeSource {
            release,
            bytes: bytes.clone(),
        });
        let launcher = Arc::new(FakeLauncher::default());
        let paths = UpdatePaths::at(directory.path().to_path_buf());
        let outcome = start_with(
            UpdatePolicy::Automatic,
            Version::parse("0.4.0").unwrap(),
            true,
            paths.clone(),
            source,
            launcher,
            Schedule {
                initial_delay: Duration::from_secs(60),
                interval: Duration::from_secs(60),
            },
        )
        .unwrap();
        let StartOutcome::Ready(controller) = outcome else {
            panic!("unexpected install at startup");
        };

        controller.request(UpdateAction::CheckNow).unwrap();
        wait_for_status(&controller, |status| {
            matches!(status, UpdateStatus::Ready(_))
        });

        let persisted = load_state(&paths.state).unwrap();
        let pending = persisted.pending.unwrap();
        assert_eq!(pending.version, "0.5.0");
        assert_eq!(
            fs::read(paths.installer(&pending.file_name).unwrap()).unwrap(),
            bytes
        );
        controller.stop();
    }

    #[test]
    fn automatic_standalone_check_stays_available_for_manual_download() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = b"installer".to_vec();
        let release = select_release(
            release_json("0.5.0", &bytes),
            &Version::parse("0.4.0").unwrap(),
        )
        .unwrap()
        .unwrap();
        let outcome = start_with(
            UpdatePolicy::Automatic,
            Version::parse("0.4.0").unwrap(),
            false,
            UpdatePaths::at(directory.path().to_path_buf()),
            Arc::new(FakeSource { release, bytes }),
            Arc::new(FakeLauncher::default()),
            Schedule::default(),
        )
        .unwrap();
        let StartOutcome::Ready(controller) = outcome else {
            panic!("unexpected install at startup");
        };

        controller.request(UpdateAction::CheckNow).unwrap();
        wait_for_status(&controller, |status| {
            matches!(status, UpdateStatus::Available(_))
        });
        controller.stop();
    }

    #[test]
    fn disabled_policy_only_checks_when_requested() {
        let directory = tempfile::tempdir().unwrap();
        let release = select_release(
            release_json("0.5.0", b"installer"),
            &Version::parse("0.4.0").unwrap(),
        )
        .unwrap()
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let outcome = start_with(
            UpdatePolicy::Disabled,
            Version::parse("0.4.0").unwrap(),
            true,
            UpdatePaths::at(directory.path().to_path_buf()),
            Arc::new(CountingSource {
                release,
                calls: Arc::clone(&calls),
            }),
            Arc::new(FakeLauncher::default()),
            Schedule {
                initial_delay: Duration::from_millis(5),
                interval: Duration::from_millis(5),
            },
        )
        .unwrap();
        let StartOutcome::Ready(controller) = outcome else {
            panic!("unexpected install at startup");
        };

        thread::sleep(Duration::from_millis(30));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        controller.request(UpdateAction::CheckNow).unwrap();
        wait_for_status(&controller, |status| {
            matches!(status, UpdateStatus::Available(_))
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        controller.stop();
    }

    #[test]
    fn verified_pending_update_launches_before_the_app_starts() {
        let directory = tempfile::tempdir().unwrap();
        let paths = UpdatePaths::at(directory.path().to_path_buf());
        fs::create_dir_all(&paths.root).unwrap();
        let bytes = b"pending installer";
        let digest = Sha256::digest(bytes);
        let digest = format_digest(&digest);
        let file_name = "audio-switch-0.5.0-setup-windows-x86_64.exe";
        fs::write(paths.installer(file_name).unwrap(), bytes).unwrap();
        save_state(
            &paths.state,
            &PersistedState {
                schema: STATE_SCHEMA,
                last_checked_unix: Some(now_unix()),
                pending: Some(PendingUpdate {
                    version: "0.5.0".into(),
                    file_name: file_name.into(),
                    notes_url: "https://github.com/2sBs/audio-switch/releases/tag/v0.5.0".into(),
                    size: bytes.len() as u64,
                    sha256: digest,
                }),
            },
        )
        .unwrap();
        let launcher = Arc::new(FakeLauncher::default());
        let outcome = start_with(
            UpdatePolicy::Automatic,
            Version::parse("0.4.0").unwrap(),
            true,
            paths.clone(),
            Arc::new(FakeSource {
                release: select_release(
                    release_json("0.5.0", bytes),
                    &Version::parse("0.4.0").unwrap(),
                )
                .unwrap()
                .unwrap(),
                bytes: bytes.to_vec(),
            }),
            launcher.clone(),
            Schedule::default(),
        )
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Installing));
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
        assert!(load_state(&paths.state).unwrap().pending.is_none());
    }

    #[test]
    fn installer_launch_failure_does_not_block_app_startup() {
        let directory = tempfile::tempdir().unwrap();
        let paths = UpdatePaths::at(directory.path().to_path_buf());
        fs::create_dir_all(&paths.root).unwrap();
        let bytes = b"pending installer";
        let digest = Sha256::digest(bytes);
        let file_name = "audio-switch-0.5.0-setup-windows-x86_64.exe";
        fs::write(paths.installer(file_name).unwrap(), bytes).unwrap();
        save_state(
            &paths.state,
            &PersistedState {
                schema: STATE_SCHEMA,
                last_checked_unix: Some(now_unix()),
                pending: Some(PendingUpdate {
                    version: "0.5.0".into(),
                    file_name: file_name.into(),
                    notes_url: "https://github.com/2sBs/audio-switch/releases/tag/v0.5.0".into(),
                    size: bytes.len() as u64,
                    sha256: format_digest(&digest),
                }),
            },
        )
        .unwrap();

        let outcome = start_with(
            UpdatePolicy::Automatic,
            Version::parse("0.4.0").unwrap(),
            true,
            paths.clone(),
            Arc::new(FakeSource {
                release: select_release(
                    release_json("0.5.0", bytes),
                    &Version::parse("0.4.0").unwrap(),
                )
                .unwrap()
                .unwrap(),
                bytes: bytes.to_vec(),
            }),
            Arc::new(FailingLauncher),
            Schedule::default(),
        )
        .unwrap();
        let StartOutcome::Ready(controller) = outcome else {
            panic!("failed installer launch should continue startup");
        };

        assert!(matches!(
            controller.snapshot().status,
            UpdateStatus::Failed(_)
        ));
        assert!(load_state(&paths.state).unwrap().pending.is_some());
        controller.stop();
    }

    #[test]
    fn invalid_saved_state_does_not_block_app_startup() {
        let directory = tempfile::tempdir().unwrap();
        let paths = UpdatePaths::at(directory.path().to_path_buf());
        fs::write(&paths.state, b"not json").unwrap();
        let bytes = b"installer".to_vec();
        let release = select_release(
            release_json("0.5.0", &bytes),
            &Version::parse("0.4.0").unwrap(),
        )
        .unwrap()
        .unwrap();

        let outcome = start_with(
            UpdatePolicy::Disabled,
            Version::parse("0.4.0").unwrap(),
            true,
            paths,
            Arc::new(FakeSource { release, bytes }),
            Arc::new(FakeLauncher::default()),
            Schedule::default(),
        )
        .unwrap();
        let StartOutcome::Ready(controller) = outcome else {
            panic!("invalid update state should continue startup");
        };

        assert!(matches!(
            controller.snapshot().status,
            UpdateStatus::Failed(_)
        ));
        controller.stop();
    }

    #[test]
    #[ignore = "requires the public GitHub API"]
    fn github_source_reads_the_public_release() {
        let release = GithubReleaseSource::new()
            .latest(&Version::parse("0.0.0").unwrap())
            .unwrap()
            .expect("the repository should have a stable release");

        assert!(release.version >= Version::parse("0.4.0").unwrap());
        assert_eq!(
            release.file_name,
            ReleaseAsset::expected_file_name(&release.version)
        );
    }
}
