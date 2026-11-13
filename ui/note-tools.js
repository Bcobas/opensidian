// SPDX-License-Identifier: GPL-3.0-or-later

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

function toolAddProperty(name) {
  const { dialog, body, status } = toolDialog("Add property · " + name);
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const key = toolNode("input");
  key.required = true;
  key.maxLength = 256;
  key.setAttribute("aria-label", "Property name");
  key.placeholder = "Property name";
  const kind = toolSelect("Property type", [["text", "Text"], ["number", "Number"], ["checkbox", "Checkbox"], ["list", "List"]]);
  const value = toolNode("textarea");
  value.rows = 2;
  value.setAttribute("aria-label", "Property value");
  const checked = toolNode("input");
  checked.type = "checkbox";
  const checkLabel = toolNode("label");
  checkLabel.append(checked, document.createTextNode(" Checked"));
  const add = toolNode("button", "Add property");
  add.type = "submit";
  const close = dialog.querySelector("header button");
  kind.onchange = () => {
    value.hidden = kind.value === "checkbox";
    checkLabel.hidden = !value.hidden;
    value.placeholder = kind.value === "list" ? "One item per line" : "Property value";
  };
  kind.onchange();
  fields.append(key, kind, value, checkLabel, add);
  form.append(fields);
  body.append(form);
  key.focus();
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    fields.disabled = close.disabled = true;
    status("");
    let content;
    try {
      await Promise.all(savesInFlight);
      await toolFlush(name);
      const data = kind.value === "checkbox" ? checked.checked
        : kind.value === "list" ? value.value.split(/\r?\n/).map(item => item.trim()).filter(Boolean)
        : value.value;
      content = await inv("add_note_property", { name, key: key.value, value: { kind: kind.value, value: data } });
    } catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
      return;
    }
    try {
      for (const group of groups()) {
        const tab = group.tabs[group.active];
        if (tab && !tab.kind && tab.name === name && !bufDirty(group)) await reloadInPlace(group, content);
      }
      await afterWrite(name);
      say("Property added to " + name);
    } catch (error) { say("Property saved. Could not refresh the workspace: " + errStr(error)); }
    dialog.close();
  };
}

async function toolMergeNote(from) {
  const { dialog, body, status } = toolDialog("Merge note · " + from);
  const notes = notesCache.filter(name => name !== from);
  if (!notes.length) { status("There are no other notes in this vault."); return; }
  const filter = toolFilter("Filter destination notes");
  const target = toolSelect("Destination note", []);
  const trash = toolNode("input");
  trash.type = "checkbox";
  const trashLabel = toolNode("label");
  trashLabel.append(trash, document.createTextNode(" Move the source note to the vault trash after merging"));
  const info = toolNode("p", "The source is appended to the destination. Relative links may need updating when the notes are in different folders.");
  const form = toolNode("form");
  const controls = toolNode("fieldset");
  const close = dialog.querySelector("header button");
  const merge = toolNode("button", "Merge into selected note");
  merge.type = "submit";
  controls.append(info, filter, target, trashLabel, merge);
  form.append(controls);
  body.append(form);
  dialog.addEventListener("cancel", event => { if (controls.disabled) event.preventDefault(); });
  filter.oninput = () => {
    const previous = target.value;
    const query = filter.value.trim().toLowerCase();
    const placeholder = toolNode("option", "Choose a destination note");
    placeholder.value = "";
    const matches = notes.filter(name => name.toLowerCase().includes(query));
    target.replaceChildren(placeholder, ...matches.map(name => {
      const option = toolNode("option", name);
      option.value = name;
      return option;
    }));
    target.value = matches.includes(previous) ? previous : "";
    target.onchange();
  };
  target.onchange = () => { merge.disabled = !target.value; };
  filter.oninput();
  filter.focus();
  form.onsubmit = async event => {
    event.preventDefault();
    const to = target.value;
    if (!to || controls.disabled) return;
    controls.disabled = close.disabled = true;
    status("");
    let result;
    try {
      await Promise.all(savesInFlight);
      await toolFlush(from);
      await toolFlush(to);
      result = await inv("merge_vault_notes", { from, to, trashSource: trash.checked });
    } catch (error) {
      status(errStr(error));
      controls.disabled = close.disabled = false;
      return;
    }
    try {
      await afterWrite(to);
      for (const group of groups()) {
        const tab = group.tabs[group.active];
        if (tab && !tab.kind && tab.name === to && !bufDirty(group)) await reloadInPlace(group, result.content);
      }
      if (result.trashed) await afterDelete(from);
      dialog.close();
      const existing = groups().find(group => group.tabs.some(tab => !tab.kind && tab.name === to));
      if (existing) {
        focusGroup(existing);
        await switchTab(existing, existing.tabs.findIndex(tab => !tab.kind && tab.name === to));
      } else {
        await openNewTab(to);
      }
      say(result.warning || "Merged into " + to);
    } catch (error) {
      dialog.close();
      say("Merged into " + to + ". Could not refresh the workspace: " + errStr(error));
    }
  };
}

async function toolHistory(name, canvas = false) {
  if (!name) return;
  const { dialog, body, status } = toolDialog("Version history · " + name);
  const args = canvas ? { path: name } : { name };
  const flush = async () => {
    if (!canvas) return toolFlush(name);
    for (const leaf of cvLeavesOf(name)) await leaf.view.flush();
  };
  const close = dialog.querySelector("header button");
  let restoring = false;
  dialog.addEventListener("cancel", event => { if (restoring) event.preventDefault(); });
  try {
    await flush();
    const revisions = await inv(canvas ? "canvas_version_history" : "note_history", args);
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
      if (restoring || expected === null || !window.confirm("Restore this version? The current version will be saved in history.")) return;
      restoring = restore.disabled = select.disabled = close.disabled = true;
      try {
        await flush();
        const text = await inv(canvas ? "restore_canvas_revision" : "restore_revision", { ...args, id: select.value, expected });
        try {
          if (canvas) {
            for (const leaf of cvLeavesOf(name)) await leaf.view.loadPath(name, false);
            cvLinkDirty();
            if (rightOpen) rSchedule();
            for (const group of groups()) if (group.graphOn && group.graphRefresh) await group.graphRefresh();
          } else {
            await afterWrite(name);
            for (const group of groups()) {
              const tab = group.tabs[group.active];
              if (tab && !tab.kind && tab.name === name && !bufDirty(group)) await reloadInPlace(group, text);
            }
          }
          say("Restored " + name);
        } catch (error) { say("Restored " + name + ". Could not refresh the workspace: " + errStr(error)); }
        dialog.close();
      } catch (error) {
        status(errStr(error));
        restoring = close.disabled = false;
        select.disabled = false;
        restore.disabled = false;
      }
    });
    select.onchange = async () => {
      const id = ++request;
      restore.disabled = true;
      expected = null;
      try {
        const [now, saved] = await inv(canvas ? "read_canvas_revision" : "read_revision", { ...args, id: select.value });
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
            const checkbox = group.preview?.querySelector(`input[data-line="${line}"]`);
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

function toolPortableExport(name) {
  if (!name) return;
  return portableExportDialog({
    prepare: toolFlush,
    inspect: selected => inv("preview_package", { name, selected }),
    write: (selected, token) => inv("export_package", { name, selected, token }),
    open: path => inv("open_package", { path: vaultPath.replace(/[\\/]$/, "") + "/" + path }),
  });
}

let packagePickerOpen = false;

function toolPackageError(error) {
  const message = errStr(error);
  if (vaultPath) say(message);
  else $("p-err").textContent = message;
}

async function toolOpenPackage() {
  if (packagePickerOpen) return;
  packagePickerOpen = true;
  try {
    if (await inv("pick_package")) return;
  } catch (error) {
    toolPackageError(error);
    return;
  } finally {
    packagePickerOpen = false;
  }
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

async function toolPackageLink(path, anchor) {
  if (!packageInfo || notesCache.includes(path) || (!path.startsWith("attachments/") && !/\.pdf$/i.test(path))) return false;
  if (/\.pdf$/i.test(path)) {
    try { await inv("open_package_pdf", { path, anchor }); }
    catch (error) { say(errStr(error)); }
  } else {
    say("Extract the package to open " + path);
  }
  return true;
}

async function toolSaveUpdatedPackage() {
  if (!packageInfo?.[2]) return;
  const { dialog, body, status } = toolDialog("Save updated package");
  const form = toolNode("form");
  const label = toolNode("label", "ZIP filename");
  const name = toolNode("input");
  name.required = true;
  name.value = packageInfo[1].replace(/\.opensidian\.zip$/i, "") + " - updated.opensidian.zip";
  label.append(name);
  const entryLabel = toolNode("label", "Main note ");
  const entry = toolSelect("Main note", []);
  entryLabel.append(entry);
  const fileList = toolNode("ul");
  const details = toolNode("details");
  const summary = toolNode("summary", "Files in the ZIP");
  details.open = true;
  details.append(summary, fileList);
  const save = toolNode("button", "Choose folder and save ZIP");
  form.append(label, entryLabel, details, save);
  body.append(form);

  let reviewed;
  async function review() {
    save.disabled = true;
    reviewed = null;
    const current = toolActiveNote();
    const preferred = [entry.value, packageInfo[0] + ".md", current && current + ".md"];
    const result = await inv("preview_updated_package");
    const options = result.notes.map(note => {
      const option = toolNode("option", note);
      option.value = note;
      return option;
    });
    entry.replaceChildren(...options);
    entry.value = preferred.find(note => result.notes.includes(note)) || result.notes[0];
    fileList.replaceChildren(...result.files.map(file => toolNode("li", file)));
    summary.textContent = `Files in the ZIP (${result.files.length})`;
    reviewed = result;
    save.disabled = false;
  }

  form.onsubmit = async event => {
    event.preventDefault();
    if (save.disabled || !reviewed) return;
    save.disabled = name.disabled = entry.disabled = true;
    status("Choose a folder for the ZIP copy…");
    try {
      const path = await inv("save_updated_package", { name: name.value, entry: entry.value, token: reviewed.token });
      if (!path) { status(""); save.disabled = false; return; }
      status("Saved to " + path);
      const open = toolButton("Open ZIP copy", async () => {
        try { await inv("open_package", { path }); dialog.close(); }
        catch (error) { status(errStr(error)); }
      });
      body.replaceChildren(open);
      open.focus();
    } catch (error) {
      try { await review(); } catch (refreshError) { status(errStr(refreshError)); return; }
      status(errStr(error));
    } finally { name.disabled = entry.disabled = false; }
  };

  save.disabled = true;
  status("Saving the current notes…");
  try { await toolFlush(); await review(); status(""); }
  catch (error) { status(errStr(error)); }
}

function toolSavePackageAsVault() {
  if (!packageInfo?.[2]) return;
  const { dialog, body, status } = toolDialog("Save package as vault");
  const form = toolNode("form");
  const label = toolNode("label", "Name of the new vault");
  const name = toolNode("input");
  name.required = true;
  name.value = packageInfo[1].replace(/\.opensidian\.zip$/i, "") || "Imported package";
  label.append(name);
  const save = toolNode("button", "Choose folder and save");
  form.append(label, save);
  body.append(form);
  form.onsubmit = async event => {
    event.preventDefault();
    if (save.disabled) return;
    save.disabled = name.disabled = true;
    status("Saving the current notes…");
    try {
      await toolFlush();
      status("Choose a folder for the new vault…");
      const path = await inv("save_package_as_vault", { name: name.value });
      if (!path) { status(""); return; }
      status("Saved to " + path);
      const open = toolButton("Open saved vault", async () => {
        try { await inv("open_vault_window", { path, replace: false }); dialog.close(); }
        catch (error) { status(errStr(error)); }
      });
      body.replaceChildren(open);
      open.focus();
    } catch (error) { status(errStr(error)); }
    finally { save.disabled = name.disabled = false; }
  };
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
