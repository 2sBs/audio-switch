const releaseApiUrl = "https://api.github.com/repos/2sBs/audio-switch/releases/latest";
const latestReleaseUrl = "https://github.com/2sBs/audio-switch/releases/latest";
const downloads = [
  { platform: "windows", label: "Windows", pattern: /^audio-switch-\d+\.\d+\.\d+-setup-windows-x86_64\.exe$/ },
  { platform: "macos", label: "macOS", pattern: /^audio-switch-\d+\.\d+\.\d+-macos-arm64\.zip$/ },
].map((download) => ({
  ...download,
  link: document.querySelector(`[data-download="${download.platform}"]`),
  status: document.querySelector(`[data-download-status="${download.platform}"]`),
}));

const releaseLinks = document.querySelectorAll("[data-release-link]");
const versionLabels = document.querySelectorAll("[data-release-version]");
const checksumLink = document.querySelector("[data-checksums-link]");

// Only link to this repository's HTTPS releases, even if an API field is malformed.
function releaseUrl(value) {
  if (typeof value !== "string") return null;
  try {
    const url = new URL(value);
    return url.origin === "https://github.com"
      && !url.username && !url.password
      && url.pathname.startsWith("/2sBs/audio-switch/releases/")
      ? url.href : null;
  } catch {
    return null;
  }
}

function setDownload(download, { url, label, message, state }) {
  download.link.href = url;
  download.link.querySelector("[data-download-label]").textContent = label;
  download.status.textContent = message;
  download.status.dataset.state = state;
}

async function resolveLatestDownloads() {
  downloads.forEach(({ status }) => {
    status.textContent = "Checking the latest release…";
  });

  try {
    const response = await fetch(releaseApiUrl, {
      headers: { Accept: "application/vnd.github+json" },
      signal: AbortSignal.timeout(8000),
    });
    if (!response.ok) throw new Error(`GitHub returned ${response.status}`);

    const release = await response.json();
    const pageUrl = releaseUrl(release?.html_url);
    if (!pageUrl || typeof release.tag_name !== "string" || !release.tag_name) {
      throw new Error("The release metadata is incomplete");
    }
    const assets = Array.isArray(release.assets) ? release.assets : [];
    releaseLinks.forEach((link) => { link.href = pageUrl; });
    versionLabels.forEach((label) => { label.textContent = release.tag_name; });
    checksumLink.href = releaseUrl(assets.find((asset) => asset?.name === "SHA256SUMS.txt")?.browser_download_url) ?? pageUrl;

    for (const download of downloads) {
      const asset = assets.find((asset) => typeof asset?.name === "string" && download.pattern.test(asset.name));
      const url = releaseUrl(asset?.browser_download_url);
      setDownload(download, url ? {
        url,
        label: `Download for ${download.label}`,
        message: `${release.tag_name} · Ready on GitHub`,
        state: "ready",
      } : {
        url: pageUrl,
        label: `${download.label} releases`,
        message: `No ${download.label} download in ${release.tag_name}.`,
        state: "unavailable",
      });
    }
  } catch (error) {
    console.warn("Could not resolve Audio Switch downloads.", error);
    for (const download of downloads) {
      setDownload(download, {
        url: latestReleaseUrl,
        label: `${download.label} releases`,
        message: "Couldn’t check downloads. See GitHub Releases.",
        state: "fallback",
      });
    }
  }
}

resolveLatestDownloads();
