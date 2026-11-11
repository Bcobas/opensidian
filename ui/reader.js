// SPDX-License-Identifier: GPL-3.0-or-later

const tauri = window.__TAURI__;
const invoke = tauri.core.invoke;
const $ = id => document.getElementById(id);
const preview = $("document");
const source = $("reader-source");
const toolbar = $("reader-toolbar");
const convert = $("reader-convert");
const edit = $("reader-edit");
const view = $("reader-preview");
const save = $("reader-save");
const copy = $("reader-copy");
const reload = $("reader-reload");
const statusLine = $("reader-status");
const discard = $("reader-discard");

let kind, savedText = "", exportedText = null;
let editing = false, converting = false, busy = false, conflict = false;
let renderId = 0;

function dirty() {
  return source.value !== savedText;
}

function setReaderStatus(message) {
  statusLine.textContent = String(message);
  statusLine.hidden = !message;
}

function updateActions() {
  const textFile = kind === "text";
  convert.hidden = !textFile || converting;
  edit.hidden = view.hidden = save.hidden = textFile && !converting;
  edit.setAttribute("aria-pressed", String(editing));
  view.setAttribute("aria-pressed", String(!editing));
  save.textContent = textFile ? "Save as Markdown" : "Save";
  save.disabled = busy || conflict || (textFile ? source.value === exportedText : !dirty());
  copy.hidden = reload.hidden = !conflict;
  copy.disabled = reload.disabled = busy;
  edit.disabled = view.disabled = busy;
  source.readOnly = busy;
}

async function showMode(toEdit) {
  const request = ++renderId;
  editing = toEdit;
  source.hidden = !editing;
  preview.hidden = editing;
  updateActions();
  if (editing) {
    source.focus();
    return;
  }
  preview.replaceChildren();
  if (kind === "text" && !converting) {
    const plain = document.createElement("pre");
    plain.className = "reader-plain";
    plain.textContent = source.value;
    preview.appendChild(plain);
    return;
  }
  try {
    const html = await invoke("preview_reader_markdown", { text: source.value });
    if (request !== renderId) return;
    // The shared Rust Markdown renderer escapes raw HTML and validates link schemes.
    preview.innerHTML = html;
    for (const checkbox of preview.querySelectorAll('input[type="checkbox"]')) checkbox.disabled = true;
  } catch (error) {
    if (request === renderId) setReaderStatus(error);
  }
}

async function perform(action) {
  if (busy) return;
  busy = true;
  updateActions();
  try {
    await action();
  } catch (error) {
    setReaderStatus(error);
  } finally {
    busy = false;
    updateActions();
    if (editing) source.focus();
  }
}

function saveCopy() {
  return perform(async () => {
    const text = source.value;
    const name = await invoke("export_reader_markdown", { text });
    savedText = text;
    if (kind === "text") exportedText = text;
    setReaderStatus("Saved " + name);
  });
}

function saveDocument() {
  if (save.disabled || save.hidden) return;
  if (kind === "text") return saveCopy();
  return perform(async () => {
    const text = source.value;
    if (await invoke("save_reader_markdown", { text }) === "changed") {
      conflict = true;
      setReaderStatus("The file changed on disk. Save a Markdown copy to keep your edits, or reload the original.");
      return;
    }
    savedText = text;
    setReaderStatus("Saved");
  });
}

function closeDocument() {
  if (busy) return;
  if (!dirty()) {
    invoke("close_reader").catch(setReaderStatus);
  } else if (!discard.open) {
    discard.returnValue = "";
    discard.showModal();
  }
}

convert.onclick = () => {
  converting = true;
  showMode(true);
};
edit.onclick = () => showMode(true);
view.onclick = () => showMode(false);
save.onclick = saveDocument;
copy.onclick = saveCopy;
reload.onclick = () => perform(async () => {
  const text = await invoke("reload_reader_document");
  source.value = savedText = text;
  conflict = false;
  setReaderStatus("Reloaded from disk");
  await showMode(editing);
});

source.addEventListener("input", () => {
  if (!conflict) setReaderStatus("");
  updateActions();
});
discard.addEventListener("close", () => {
  if (discard.returnValue === "discard") invoke("close_reader").catch(setReaderStatus);
});
document.addEventListener("keydown", event => {
  if (!kind || discard.open || !(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "s") return;
  event.preventDefault();
  saveDocument();
});

const dark = matchMedia("(prefers-color-scheme: dark)");
function syncTheme() {
  document.body.classList.toggle("theme-light", !dark.matches);
}
syncTheme();
dark.addEventListener("change", syncTheme);

preview.addEventListener("click", event => {
  const link = event.target.closest("a");
  if (!link) return;
  event.preventDefault();
  if (link.classList.contains("ext")) {
    invoke("open_reader_link", { url: link.getAttribute("href") }).catch(setReaderStatus);
  } else if (link.dataset.note) {
    invoke("open_reader_document", { target: link.dataset.note }).catch(setReaderStatus);
  } else if (link.dataset.anchor) {
    const anchor = link.dataset.anchor.toLowerCase();
    const heading = [...preview.querySelectorAll("h1,h2,h3,h4,h5,h6")].find(node => {
      const text = node.textContent.trim().toLowerCase();
      return node.id.toLowerCase() === anchor || text === anchor || text.replaceAll(" ", "-") === anchor;
    });
    heading?.scrollIntoView();
  }
});

async function openDocument() {
  await tauri.event.listen("reader-close-requested", closeDocument);
  const opened = await invoke("standalone_document");
  kind = opened.kind;
  source.value = savedText = opened.text;
  toolbar.hidden = false;
  await showMode(false);
}

openDocument().catch(setReaderStatus);
