const releaseApiUrl = "https://api.github.com/repos/2sBs/audio-switch/releases/latest";
const latestReleaseUrl = "https://github.com/2sBs/audio-switch/releases/latest";
const installerPattern = /^audio-switch-\d+\.\d+\.\d+-setup-windows-x86_64\.exe$/;

const downloadLinks = document.querySelectorAll("[data-download-link]");
const releaseLinks = document.querySelectorAll("[data-release-link]");
const versionLabels = document.querySelectorAll("[data-release-version]");
const releaseStatus = document.querySelector("[data-release-status]");

function useReleasePageFallback() {
  downloadLinks.forEach((link) => {
    link.href = latestReleaseUrl;
  });
  releaseStatus.textContent = "Direct download unavailable. The button opens the latest release.";
  releaseStatus.dataset.state = "fallback";
}

async function resolveLatestInstaller() {
  try {
    const response = await fetch(releaseApiUrl, {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!response.ok) {
      throw new Error(`GitHub returned ${response.status}`);
    }

    const release = await response.json();
    const installer = release.assets?.find((asset) => installerPattern.test(asset.name));
    if (!installer?.browser_download_url || !release.html_url || !release.tag_name) {
      throw new Error("The latest release does not contain the Windows installer");
    }

    downloadLinks.forEach((link) => {
      link.href = installer.browser_download_url;
    });
    releaseLinks.forEach((link) => {
      link.href = release.html_url;
    });
    versionLabels.forEach((label) => {
      label.textContent = release.tag_name;
    });
    releaseStatus.textContent = `${release.tag_name} installer ready. The download is served by GitHub.`;
    releaseStatus.dataset.state = "ready";
  } catch (error) {
    console.warn("Could not resolve the latest Audio Switch installer.", error);
    useReleasePageFallback();
  }
}

resolveLatestInstaller();
