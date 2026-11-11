// SPDX-License-Identifier: GPL-3.0-or-later

const invoke = window.__TAURI__.core.invoke;
const reader = document.getElementById("document");
const readerStatus = document.getElementById("reader-status");

function showReaderError(error) {
  readerStatus.textContent = String(error);
  readerStatus.hidden = false;
}

const dark = matchMedia("(prefers-color-scheme: dark)");
function syncTheme() {
  document.body.classList.toggle("theme-light", !dark.matches);
}
syncTheme();
dark.addEventListener("change", syncTheme);

reader.addEventListener("click", event => {
  const link = event.target.closest("a");
  if (!link) return;
  event.preventDefault();
  if (link.classList.contains("ext")) {
    invoke("open_reader_link", { url: link.getAttribute("href") }).catch(showReaderError);
  }
});

invoke("standalone_markdown").then(html => {
  // The shared Rust Markdown renderer escapes raw HTML and validates link schemes.
  reader.innerHTML = html;
  for (const checkbox of reader.querySelectorAll('input[type="checkbox"]')) {
    checkbox.disabled = true;
  }
}, showReaderError);
