// SPDX-License-Identifier: GPL-3.0-or-later

function toolNode(tag, text) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  return node;
}

function toolDialog(title) {
  const dialog = toolNode("dialog");
  dialog.className = "note-tool";
  dialog.setAttribute("aria-label", title);
  const header = toolNode("header");
  const close = toolButton("Close", () => dialog.close());
  header.append(toolNode("h2", title), close);
  const status = toolNode("p");
  status.setAttribute("role", "status");
  status.hidden = true;
  const body = toolNode("section");
  dialog.append(header, status, body);
  dialog.addEventListener("close", () => dialog.remove(), { once: true });
  document.body.append(dialog);
  dialog.showModal();
  return { dialog, body, status(message) { status.textContent = message || ""; status.hidden = !message; } };
}

function toolButton(label, action) {
  const button = toolNode("button", label);
  button.type = "button";
  button.onclick = action;
  return button;
}

async function portableExportDialog({ prepare, inspect, write, open, description }) {
  const { dialog, body, status } = toolDialog("Export note with its files");
  const chosen = new Set();
  let request = 0, reviewed = null;
  const linked = toolNode("fieldset");
  const files = toolNode("div");
  const warnings = toolNode("div");
  const message = error => error instanceof Error ? error.message : String(error);
  const save = toolButton("Export ZIP", async () => {
    if (!reviewed) return;
    save.disabled = linked.disabled = true;
    status("Writing package…");
    try {
      const path = await write([...chosen], reviewed.token);
      status("Saved " + path);
      const openButton = toolButton("Open exported package", async () => {
        try { await open(path); dialog.close(); }
        catch (error) { status(message(error)); }
      });
      body.append(openButton);
      openButton.focus();
    } catch (error) {
      await refresh();
      status(message(error));
    }
  });
  function showList(container, title, items) {
    const list = toolNode("ul");
    for (const item of items) list.append(toolNode("li", item));
    container.replaceChildren(toolNode("h3", title), list);
    container.hidden = !items.length;
  }
  async function refresh() {
    const id = ++request;
    save.disabled = true;
    reviewed = null;
    status("Checking references…");
    try {
      const result = await inspect([...chosen]);
      if (id !== request) return;
      linked.replaceChildren(toolNode("legend", "Include linked notes"));
      const options = [...new Set([...result.linked, ...chosen])].sort();
      for (const note of options) {
        const label = toolNode("label");
        const input = toolNode("input");
        input.type = "checkbox";
        input.checked = chosen.has(note);
        input.onchange = () => {
          if (input.checked) chosen.add(note);
          else chosen.delete(note);
          void refresh();
        };
        label.append(input, document.createTextNode(" " + note));
        linked.append(label);
      }
      linked.hidden = !options.length;
      showList(files, "Files in the ZIP", result.files);
      showList(warnings, "References that will stay unresolved", result.omitted);
      reviewed = result;
      save.disabled = linked.disabled = false;
      status("");
    } catch (error) {
      if (id === request) { linked.disabled = false; status(message(error)); }
    }
  }
  save.disabled = true;
  linked.hidden = files.hidden = warnings.hidden = true;
  if (description) body.append(toolNode("p", description));
  body.append(linked, files, warnings, save);
  try { if (prepare) await prepare(); await refresh(); }
  catch (error) { status(message(error)); }
}
