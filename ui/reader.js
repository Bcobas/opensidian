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
const pack = $("reader-package");
const copy = $("reader-copy");
const reload = $("reader-reload");
const statusLine = $("reader-status");
const discard = $("reader-discard");
const pdfPane = $("reader-pdf");
const documentPane = $("reader-document-pane");
const ranges = $("reader-ranges");
const useOcr = $("reader-use-ocr");
const ocrLanguage = $("reader-ocr-language");
const pageNumber = $("reader-page-number");
const pageImage = $("reader-page-image");
const pageError = $("reader-page-error");
const prev = $("reader-prev");
const next = $("reader-next");

let kind, savedText = "", exportedText = null;
let editing = false, converting = false, busy = false, conflict = false;
let pages = 0, currentPage = 1, discardAction;
let renderId = 0;

function dirty() {
  return (kind === "pdf" && converting && exportedText === null) || source.value !== savedText;
}

function setReaderStatus(message) {
  statusLine.textContent = String(message);
  statusLine.hidden = !message;
}

function updateActions() {
  const copyOnly = kind !== "markdown";
  const pdf = kind === "pdf";
  convert.hidden = !pdf && (!copyOnly || converting);
  edit.hidden = view.hidden = save.hidden = copyOnly && !converting;
  documentPane.hidden = pdf && !converting;
  edit.setAttribute("aria-pressed", String(editing));
  view.setAttribute("aria-pressed", String(!editing));
  save.textContent = copyOnly ? "Save as Markdown" : "Save";
  save.disabled = busy || conflict || (copyOnly ? source.value === exportedText : !dirty());
  pack.hidden = copyOnly;
  pack.disabled = busy;
  copy.hidden = reload.hidden = !conflict;
  copy.disabled = reload.disabled = busy;
  edit.disabled = view.disabled = busy;
  convert.disabled = ranges.disabled = pageNumber.disabled = busy;
  useOcr.disabled = busy;
  ocrLanguage.disabled = busy || !useOcr.checked;
  prev.disabled = busy || currentPage <= 1;
  next.disabled = busy || currentPage >= pages;
  source.readOnly = busy;
}

function showPage(number) {
  if (!Number.isInteger(number) || number < 1 || number > pages) {
    pageNumber.value = currentPage;
    return;
  }
  currentPage = number;
  pageNumber.value = number;
  pageError.hidden = true;
  pageImage.hidden = false;
  pageImage.alt = `PDF page ${number}`;
  pageImage.src = `http://opensidian-img.localhost/reader-page?page=${number}&w=1600`;
  updateActions();
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
    if (kind !== "markdown") exportedText = text;
    setReaderStatus("Saved " + name);
  });
}

function saveDocument() {
  if (save.disabled || save.hidden) return;
  if (kind !== "markdown") return saveCopy();
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

function confirmDiscard(title, action) {
  if (!dirty()) return action();
  if (!discard.open) {
    discardAction = action;
    $("reader-discard-title").textContent = title;
    discard.returnValue = "";
    discard.showModal();
  }
}

function closeDocument() {
  if (busy) return;
  confirmDiscard("Discard your unsaved changes?", () => invoke("close_reader").catch(setReaderStatus));
}

function pageSummary(pages) {
  const listed = pages.slice(0, 12).join(", ");
  return listed + (pages.length > 12 ? ` and ${pages.length - 12} more` : "");
}

function convertPdf() {
  return confirmDiscard("Replace your unsaved Markdown draft?", () => perform(async () => {
    const language = useOcr.checked ? ocrLanguage.value : null;
    setReaderStatus(language ? "Extracting PDF text and recognizing scanned pages…" : "Extracting PDF text…");
    const result = await invoke("convert_reader_pdf", { selection: ranges.value, ocrLanguage: language });
    converting = true;
    exportedText = null;
    source.value = savedText = result.text;
    await showMode(true);
    const messages = [];
    if (result.recognized.length) messages.push(`OCR recognized text on pages ${pageSummary(result.recognized)}.`);
    if (result.omitted.length) {
      const missing = language ? "No text found" : "No selectable text";
      messages.push(`${missing} on pages ${pageSummary(result.omitted)}.`);
    }
    setReaderStatus(messages.join(" "));
  }));
}

convert.onclick = () => {
  if (kind === "pdf") return convertPdf();
  converting = true;
  showMode(true);
};
prev.onclick = () => showPage(currentPage - 1);
next.onclick = () => showPage(currentPage + 1);
pageNumber.onchange = () => showPage(pageNumber.valueAsNumber);
pageNumber.addEventListener("keydown", event => {
  if (event.key === "Enter") showPage(pageNumber.valueAsNumber);
});
ranges.addEventListener("keydown", event => {
  if (event.key === "Enter" && !convert.disabled) convertPdf();
});
useOcr.onchange = updateActions;
pageImage.onerror = () => {
  pageImage.hidden = true;
  pageError.hidden = false;
};
edit.onclick = () => showMode(true);
view.onclick = () => showMode(false);
save.onclick = saveDocument;
pack.onclick = () => {
  const text = source.value;
  return portableExportDialog({
    description: dirty() ? "The ZIP will include the current draft. Save separately to update the original." : null,
    inspect: selected => invoke("preview_reader_package", { text, selected }),
    write: (selected, token) => invoke("export_reader_package", { text, selected, token }),
    open: filename => invoke("open_reader_package", { filename }),
  });
};
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
  const action = discardAction;
  discardAction = null;
  if (discard.returnValue === "discard") action?.();
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
  if (kind === "pdf") {
    pages = opened.pages;
    pageNumber.max = pages;
    $("reader-page-count").textContent = `/ ${pages}`;
    $("reader-pdf-selection").hidden = false;
    $("reader-ocr-options").hidden = false;
    pdfPane.hidden = false;
    showPage(opened.page || 1);
    return;
  }
  await showMode(false);
}

openDocument().catch(setReaderStatus);
