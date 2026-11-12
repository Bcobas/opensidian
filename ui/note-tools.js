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

function toolSelect(label, entries) {
  const select = toolNode("select");
  select.setAttribute("aria-label", label);
  for (const [value, text] of entries) {
    const option = toolNode("option", text);
    option.value = value;
    select.append(option);
  }
  return select;
}

function toolFilter(label) {
  const input = toolNode("input");
  input.type = "search";
  input.placeholder = label;
  input.setAttribute("aria-label", label);
  return input;
}

function toolActiveNote() {
  const group = state && fg();
  const tab = group && group.tabs[group.active];
  return tab && !tab.kind ? tab.name : null;
}

async function toolFlush(note) {
  for (const group of groups()) {
    const tab = group.tabs[group.active];
    if (!tab || tab.kind || (note && tab.name !== note)) continue;
    await flushSave(group);
    if (bufDirty(group)) throw new Error("Save the current edits before continuing.");
  }
}

async function toolHistory(name) {
  if (!name) return;
  const { dialog, body, status } = toolDialog("Version history · " + name);
  try {
    await toolFlush(name);
    const revisions = await inv("note_history", { name });
    if (!revisions.length) return status("No saved versions yet.");
    const format = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });
    const select = toolSelect("Saved version", revisions.map(row => [row.id, `${format.format(row.timestamp)} · ${row.bytes} bytes`]));
    const compare = toolNode("div");
    compare.className = "note-tool-compare";
    const panes = ["Current", "Saved version"].map(title => {
      const label = toolNode("label", title);
      const text = toolNode("textarea");
      text.readOnly = true;
      label.append(text);
      compare.append(label);
      return text;
    });
    let expected = null, request = 0;
    const restore = toolButton("Restore selected version", async () => {
      if (expected === null || !window.confirm("Restore this version? The current version will be saved in history.")) return;
      restore.disabled = select.disabled = true;
      try {
        await toolFlush(name);
        const text = await inv("restore_revision", { name, id: select.value, expected });
        await afterWrite(name);
        for (const group of groups()) {
          const tab = group.tabs[group.active];
          if (tab && !tab.kind && tab.name === name && !bufDirty(group)) await reloadInPlace(group, text);
        }
        dialog.close();
        say("Restored " + name);
      } catch (error) {
        status(errStr(error));
        select.disabled = false;
        restore.disabled = false;
      }
    });
    select.onchange = async () => {
      const id = ++request;
      restore.disabled = true;
      expected = null;
      try {
        const [now, saved] = await inv("read_revision", { name, id: select.value });
        if (request !== id) return;
        panes[0].value = expected = now;
        panes[1].value = saved;
        status("");
        restore.disabled = now === saved;
      } catch (error) {
        if (request === id) status(errStr(error));
      }
    };
    body.append(select, compare, restore);
    await select.onchange();
  } catch (error) { status(errStr(error)); }
}

async function toolDaily() {
  const now = new Date();
  const date = [now.getFullYear(), now.getMonth() + 1, now.getDate()].map(n => String(n).padStart(2, "0")).join("-");
  const name = "Daily/" + date;
  if (await createNote(name, `# ${date}\n`) !== "err") await openInTab(name);
}

async function toolTasks() {
  const { dialog, body, status } = toolDialog("Vault tasks");
  try {
    await toolFlush();
    const tasks = await inv("vault_tasks");
    const filter = toolFilter("Filter tasks or notes");
    const stateFilter = toolSelect("Task status", [["open", "Open"], ["all", "All"], ["done", "Done"]]);
    const list = toolNode("ul");
    list.className = "note-tool-list";
    const render = () => {
      const query = filter.value.trim().toLocaleLowerCase();
      const matches = tasks.filter(task =>
        (stateFilter.value === "all" || task.checked === (stateFilter.value === "done")) &&
        `${task.text} ${task.note}`.toLocaleLowerCase().includes(query));
      list.replaceChildren();
      for (const task of matches) {
        const row = toolNode("li");
        const open = toolButton(task.text || "Untitled task", async () => {
          dialog.close();
          await openInTab(task.note);
          const group = fg();
          if (group.tabs[group.active]?.name !== task.note) return;
          const line = task.line - 1;
          if (isLp(group.tabs[group.active].mode)) {
            Ed.revealAt(group, line, 0);
            Ed.flashLine(group, line);
          } else {
            const checkbox = group.preview?.querySelector(`input[data-line="${task.line}"]`);
            checkbox?.scrollIntoView({ block: "center" });
            flash(checkbox?.closest("li"));
          }
        });
        row.append(open, toolNode("span", `${task.checked ? "Done" : "Open"} · ${task.note}:${task.line}`));
        list.append(row);
      }
      status(matches.length ? `${matches.length} tasks` : "No tasks match.");
    };
    filter.oninput = stateFilter.onchange = render;
    body.append(filter, stateFilter, list);
    render();
  } catch (error) { status(errStr(error)); }
}

async function toolNoteTable() {
  const { dialog, body, status } = toolDialog("Note table");
  try {
    await toolFlush();
    const rows = await inv("vault_note_rows");
    const keys = [...new Set(rows.flatMap(row => Object.keys(row.values)))].sort();
    const filter = toolFilter("Filter notes or property values");
    const table = toolNode("table");
    const head = table.createTHead().insertRow();
    const data = table.createTBody();
    const columns = [null, ...keys];
    const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
    let sortKey = null, direction = 1;
    const value = (row, key) => key === null ? row.name : (Object.hasOwn(row.values, key) ? row.values[key] : "");
    const render = () => {
      const query = filter.value.trim().toLocaleLowerCase();
      const matches = rows.filter(row => columns.some(key => value(row, key).toLocaleLowerCase().includes(query)));
      matches.sort((a, b) => direction * collator.compare(value(a, sortKey), value(b, sortKey)) || collator.compare(a.name, b.name));
      data.replaceChildren();
      for (const row of matches) {
        const item = data.insertRow();
        item.insertCell().append(toolButton(row.name, () => { dialog.close(); void openInTab(row.name); }));
        for (const key of keys) item.insertCell().textContent = value(row, key);
      }
      [...head.cells].forEach((cell, i) => cell.setAttribute("aria-sort", columns[i] === sortKey ? (direction === 1 ? "ascending" : "descending") : "none"));
      status(matches.length ? `${matches.length} notes` : "No notes match.");
    };
    for (const key of columns) {
      const cell = toolNode("th");
      cell.scope = "col";
      cell.append(toolButton(key === null ? "Note" : key, () => {
        direction = sortKey === key ? -direction : 1;
        sortKey = key;
        render();
      }));
      head.append(cell);
    }
    filter.oninput = render;
    body.append(filter, table);
    render();
  } catch (error) { status(errStr(error)); }
}

async function toolPortableExport(name) {
  if (!name) return;
  const { dialog, body, status } = toolDialog("Export note with its files");
  const chosen = new Set();
  let request = 0, reviewed = null;
  const linked = toolNode("fieldset");
  const files = toolNode("div");
  const warnings = toolNode("div");
  const save = toolButton("Export ZIP", async () => {
    if (!reviewed) return;
    save.disabled = linked.disabled = true;
    try {
      const path = await inv("export_package", { name, selected: [...chosen], token: reviewed.token });
      status("Saved " + path);
      const full = vaultPath.replace(/[\\/]$/, "") + "/" + path;
      body.append(toolButton("Open exported package", async () => {
        try { await inv("open_package", { path: full }); dialog.close(); }
        catch (error) { status(errStr(error)); }
      }));
    } catch (error) {
      await refresh();
      status(errStr(error));
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
      const result = await inv("preview_package", { name, selected: [...chosen] });
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
      if (id === request) { linked.disabled = false; status(errStr(error)); }
    }
  }
  save.disabled = true;
  body.append(linked, files, warnings, save);
  try { await toolFlush(); await refresh(); }
  catch (error) { status(errStr(error)); }
}

function toolOpenPackage() {
  const { dialog, body, status } = toolDialog("Open portable package");
  const form = toolNode("form");
  const label = toolNode("label", "Full path to the .opensidian.zip file");
  const input = toolNode("input");
  input.required = true;
  label.append(input);
  const open = toolNode("button", "Open package");
  form.append(label, open);
  body.append(form);
  form.onsubmit = async event => {
    event.preventDefault();
    open.disabled = true;
    try { await inv("open_package", { path: input.value }); dialog.close(); }
    catch (error) { status(errStr(error)); open.disabled = false; }
  };
}

async function toolPackageLink(path) {
  if (!packageInfo || !path.startsWith("attachments/")) return false;
  if (/\.pdf$/i.test(path)) {
    try { await inv("open_package_pdf", { path }); }
    catch (error) { say(errStr(error)); }
  } else {
    say("Extract the package to open " + path);
  }
  return true;
}

function toolWebCapture() {
  const { dialog, body, status } = toolDialog("Capture web content");
  const form = toolNode("form");
  const field = (name, type) => {
    const label = toolNode("label", name);
    const input = toolNode("input");
    input.type = type;
    input.required = true;
    label.append(input);
    form.append(label);
    return input;
  };
  const url = field("Source URL", "url");
  const title = field("Note title", "text");
  const excerptLabel = toolNode("label", "Paste a selection, or leave blank to capture the page");
  const excerpt = toolNode("textarea");
  excerpt.rows = 6;
  excerptLabel.append(excerpt);
  let pasted = null;
  excerpt.onpaste = event => {
    const html = event.clipboardData?.getData("text/html");
    if (!html || excerpt.value) return;
    event.preventDefault();
    pasted = html;
    excerpt.value = event.clipboardData.getData("text/plain") || "[Formatted selection]";
  };
  excerpt.oninput = () => { pasted = null; };
  const imageLabel = toolNode("label");
  const images = toolNode("input");
  images.type = "checkbox";
  imageLabel.append(images, document.createTextNode(" Save images with the note"));
  const submit = toolNode("button", "Save capture");
  form.append(excerptLabel, imageLabel, submit);
  body.append(form);
  form.onsubmit = async event => {
    event.preventDefault();
    if (submit.disabled) return;
    submit.disabled = true;
    status("Capturing content…");
    const escape = value => value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
    const text = excerpt.value.trim();
    const fragment = pasted || (text ? text.split(/\n\s*\n/).map(part => `<p>${escape(part).replaceAll("\n", "<br>")}</p>`).join("") : null);
    try {
      const [name, omitted] = await inv("capture_web", { title: title.value, url: url.value, fragment, images: images.checked });
      await afterWrite(name);
      dialog.close();
      await openInTab(name);
      if (omitted) say(`${omitted} images could not be included.`);
    } catch (error) { status(errStr(error)); }
    finally { submit.disabled = false; }
  };
}
