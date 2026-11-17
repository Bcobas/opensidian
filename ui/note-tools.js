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
    if (windowsFileActions) body.prepend(toolButton("Save as base", () => {
      dialog.close();
      void toolCreateBase("", {
        columns: ["file.name", ...keys.map(key => "note." + key)],
        sort: sortKey === null ? "file.name" : "note." + sortKey,
        descending: direction < 0
      });
    }));
    render();
  } catch (error) { status(errStr(error)); }
}

const baseFileProperties = [["file.name", "Note"], ["file.path", "Path"], ["file.folder", "Folder"], ["file.ext", "Extension"]];

function toolTrashBase(path, onTrashed = say) {
  const { dialog, body, status } = toolDialog("Move base to trash");
  const cancel = dialog.querySelector("header button");
  cancel.textContent = "Cancel";
  const move = toolButton("Move to trash", async () => {
    if (move.disabled) return;
    cancel.disabled = move.disabled = true;
    status("");
    let destination;
    try { destination = await inv("trash_vault_base", { path }); }
    catch (error) {
      status(errStr(error));
      cancel.disabled = move.disabled = false;
      return;
    }
    dialog.close();
    onTrashed("Moved to " + destination);
    try { await refreshTree(); }
    catch (error) { say(errStr(error)); }
  });
  body.append(toolNode("p", path + " will be moved to .trash in this vault."), move);
  dialog.addEventListener("cancel", event => { if (move.disabled) event.preventDefault(); });
  cancel.focus();
}

async function toolBases() {
  const { dialog, body, status } = toolDialog("Bases");
  const create = toolButton("Create a base", () => {
    dialog.close();
    void toolCreateBase("");
  });
  body.append(create);
  status("Loading saved bases…");
  try {
    const paths = await inv("vault_base_paths");
    if (!dialog.open) return;
    if (!paths.length) { status("No saved bases in this vault."); return; }
    const filter = toolFilter("Search saved bases");
    const list = toolNode("ul");
    list.className = "note-tool-list";
    let entries = paths.map(path => {
      const row = toolNode("li");
      row.append(toolButton(path, () => {
        dialog.close();
        void toolOpenBase(path);
      }));
      const trash = toolButton("Move to trash", () => {
        toolTrashBase(path, message => {
          entries = entries.filter(entry => entry.row !== row);
          filter.oninput();
          status(message);
          (entries.length ? filter : create).focus();
        });
      });
      trash.setAttribute("aria-label", "Move " + path + " to trash");
      row.append(trash);
      return { row, key: path.toLocaleLowerCase() };
    });
    filter.oninput = () => {
      const query = filter.value.trim().toLocaleLowerCase();
      const matches = entries.filter(entry => entry.key.includes(query));
      list.replaceChildren(...matches.map(entry => entry.row));
      filter.hidden = !entries.length;
      status(matches.length ? "" : entries.length ? "No saved bases match." : "No saved bases in this vault.");
    };
    body.append(filter, list);
    filter.oninput();
  } catch (error) { if (dialog.open) status(errStr(error)); }
}

function basePropertyOptions(properties) {
  return [...baseFileProperties, ...properties.map(key => ["note." + key, key])];
}

function baseGroupingFields(options, current, manual = false) {
  const groupBy = toolSelect("Group by", [["", "No grouping"], ...options]);
  const order = [["ASC", "Ascending"], ["DESC", "Descending"]];
  if (manual) order.unshift(["manual", "Existing manual order"]);
  const groupDirection = toolSelect("Group order", order);
  groupBy.value = current?.property ?? "";
  groupDirection.value = manual ? "manual" : current?.direction ?? "ASC";
  groupBy.onchange = () => {
    groupDirection.hidden = !groupBy.value;
    if (manual) {
      groupDirection.options[0].disabled = groupBy.value !== current.property;
      if (groupDirection.options[0].disabled && groupDirection.value === "manual") {
        groupDirection.value = current.direction;
      }
    }
  };
  groupBy.onchange();
  const label = toolNode("label", "Group by");
  label.append(groupBy);
  return {
    label, groupDirection,
    read: () => groupBy.value ? { property: groupBy.value, direction: groupDirection.value } : null
  };
}

function baseConditionFields(properties, initial = []) {
  const conditions = [];
  const filters = toolNode("fieldset");
  filters.append(toolNode("legend", "Property filters"));
  const options = [...new Set([...properties, ...initial.map(condition => condition.property)])].sort();
  const addCondition = toolButton("Add condition", () => appendCondition().focus());
  function appendCondition(initial = {}) {
    const group = toolNode("div");
    const property = toolSelect("Filter property", options.map(key => [key, key]));
    const kind = toolSelect("Value type", [["text", "Text"], ["number", "Number"], ["checkbox", "Checkbox"]]);
    const operator = toolSelect("Comparison", [["eq", "Equals"], ["ne", "Does not equal"],
      ["gt", "Greater than"], ["ge", "At least"], ["lt", "Less than"], ["le", "At most"]]);
    const value = toolNode("input");
    value.placeholder = "Filter value";
    value.setAttribute("aria-label", "Filter value");
    const checkbox = toolSelect("Checkbox value", [["true", "True"], ["false", "False"]]);
    property.value = initial.property ?? options[0] ?? "";
    kind.value = initial.kind ?? "text";
    operator.value = initial.operator ?? "eq";
    if (initial.kind === "checkbox") checkbox.value = initial.value;
    else value.value = initial.value ?? "";
    kind.onchange = () => {
      const isNumber = kind.value === "number";
      for (const option of operator.options) option.disabled = !isNumber && !["eq", "ne"].includes(option.value);
      if (operator.selectedOptions[0]?.disabled) operator.value = "eq";
      value.hidden = kind.value === "checkbox";
      value.required = isNumber;
      value.inputMode = isNumber ? "decimal" : "text";
      checkbox.hidden = !value.hidden;
    };
    kind.onchange();
    const condition = () => ({
      property: property.value, kind: kind.value, operator: operator.value,
      value: kind.value === "checkbox" ? checkbox.value : value.value
    });
    const remove = toolButton("Remove condition", () => {
      conditions.splice(conditions.indexOf(condition), 1);
      group.remove();
      addCondition.disabled = false;
      addCondition.focus();
    });
    group.append(property, kind, operator, value, checkbox, remove);
    filters.insertBefore(group, addCondition);
    conditions.push(condition);
    addCondition.disabled = conditions.length >= 16;
    return property;
  }
  addCondition.disabled = !options.length;
  filters.append(addCondition);
  initial.forEach(appendCondition);
  return { filters, read: () => conditions.map(read => read()) };
}

async function toolCreateBase(folder, preset) {
  const { dialog, body, status } = toolDialog("Create a base");
  try {
    await toolFlush();
    const rows = await inv("vault_note_rows");
    if (!dialog.open) return;
    const properties = [...new Set(rows.flatMap(row => Object.keys(row.values)))].sort().slice(0, 120);
    const form = toolNode("form");
    const fields = toolNode("fieldset");
    const filename = toolNode("input");
    filename.placeholder = "Base name";
    filename.setAttribute("aria-label", "Base name");
    filename.required = true;
    const columns = toolNode("select");
    columns.multiple = true;
    columns.size = 6;
    columns.setAttribute("aria-label", "Visible columns");
    const defaultColumns = new Set(preset?.columns || ["file.name"]);
    const options = basePropertyOptions(properties);
    for (const [key, label] of options) {
      const option = toolNode("option", label);
      option.value = key;
      option.selected = defaultColumns.has(key);
      columns.append(option);
    }
    const sort = toolSelect("Sort by", []);
    const direction = toolSelect("Sort direction", [["asc", "Ascending"], ["desc", "Descending"]]);
    direction.value = preset?.descending ? "desc" : "asc";
    const grouping = baseGroupingFields(options);
    const scope = toolNode("input");
    scope.type = "checkbox";
    const scopeLabel = toolNode("label");
    scopeLabel.append(scope, document.createTextNode(" Include only notes in this folder and its subfolders"));
    const conditions = baseConditionFields(properties);
    const save = toolNode("button", "Create base");
    save.type = "submit";
    const close = dialog.querySelector("header button");
    const selected = () => [...columns.selectedOptions].map(option => [option.value, option.textContent]);
    columns.onchange = () => {
      const previous = sort.value || preset?.sort;
      const choices = selected();
      sort.replaceChildren(...choices.map(([key, label]) => {
        const option = toolNode("option", label);
        option.value = key;
        return option;
      }));
      if (choices.some(([key]) => key === previous)) sort.value = previous;
      save.disabled = choices.length === 0;
    };
    columns.onchange();
    const columnLabel = toolNode("label", "Visible columns");
    columnLabel.append(columns);
    fields.append(filename, columnLabel, sort, direction, grouping.label, grouping.groupDirection);
    if (folder) fields.append(scopeLabel);
    if (properties.length) fields.append(conditions.filters);
    fields.append(save);
    form.append(fields);
    body.append(form);
    filename.focus();
    dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
    form.onsubmit = async event => {
      event.preventDefault();
      if (fields.disabled || save.disabled) return;
      const name = filename.value.trim().replace(/\.base$/i, "");
      if (!name || /[\\/]/.test(name)) { status("Enter a file name without a folder path."); return; }
      const path = (folder ? folder + "/" : "") + name + ".base";
      const config = { columns: selected().map(([key]) => key), sort: sort.value, descending: direction.value === "desc",
        folder: folder && scope.checked ? folder : null, conditions: conditions.read(),
        group_by: grouping.read() };
      fields.disabled = close.disabled = true;
      status("");
      try { await inv("create_vault_base", { path, config }); }
      catch (error) {
        status(errStr(error));
        fields.disabled = close.disabled = false;
        return;
      }
      dialog.close();
      if (folder) unfold.add(folder);
      lsTouch();
      try { await refreshTree(); } catch (error) { say(errStr(error)); }
      void toolOpenBase(path);
    };
  } catch (error) { status(errStr(error)); }
}

function baseViewPropertyOptions(base, view) {
  const properties = new Set(base.properties);
  for (const key of [...view.columns, ...Object.keys(view.labels ?? {}), view.group_by?.property, ...view.sort.map(rule => rule.property)]) {
    if (key?.startsWith("note.")) properties.add(key.slice(5));
  }
  return basePropertyOptions([...properties].sort()).map(([key, name]) => [key, view.labels?.[key] ?? name]);
}

function toolAddBaseView(base, view, write) {
  const { dialog, body, status } = toolDialog("Add table view");
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const name = toolNode("input");
  name.required = true;
  const nameLabel = toolNode("label", "View name");
  nameLabel.append(name);
  const copy = toolNode("input");
  copy.type = "checkbox";
  const copyLabel = toolNode("label");
  copyLabel.append(copy, document.createTextNode(" Copy settings from " + view.title));
  const properties = baseViewPropertyOptions(base, view)
    .filter(([key]) => key.startsWith("note.")).map(([key]) => key.slice(5));
  const conditions = baseConditionFields(properties);
  const save = toolNode("button", "Add view");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  fields.append(nameLabel, copyLabel);
  if (properties.length) fields.append(conditions.filters);
  fields.append(save);
  form.append(fields);
  body.append(toolNode("p", "Adding a view rewrites the YAML formatting and removes comments."), form);
  name.focus();
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    const title = name.value.trim();
    if (!title) { status("Enter a view name."); return; }
    fields.disabled = close.disabled = true;
    status("");
    try {
      await write({ name: title, copy_from: copy.checked ? view.id : null, conditions: conditions.read() });
      dialog.close();
    } catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
    }
  };
}

function toolEditBaseView(view, write) {
  const { dialog, body, status } = toolDialog("Edit view · " + view.title);
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const name = toolNode("input");
  name.required = true;
  name.value = view.title;
  const nameLabel = toolNode("label", "View name");
  nameLabel.append(name);
  const first = toolNode("input");
  first.type = "checkbox";
  first.checked = first.disabled = view.id === 0;
  const firstLabel = toolNode("label");
  firstLabel.append(first, document.createTextNode(first.checked ? " Opens first" : " Open this view first"));
  const initialLimit = String(view.limit ?? "");
  const limit = toolNode("input");
  limit.inputMode = "numeric";
  limit.placeholder = "All matching notes";
  limit.value = initialLimit;
  const limitLabel = toolNode("label", "Result limit");
  limitLabel.append(limit);
  const save = toolNode("button", "Save view");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  fields.append(nameLabel, firstLabel, limitLabel, save);
  form.append(fields);
  body.append(toolNode("p", "Saving a change rewrites the YAML formatting and removes comments."), form);
  name.focus();
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    if (!name.value.trim()) { status("Enter a view name."); return; }
    fields.disabled = close.disabled = true;
    status("");
    try {
      await write({
        name: name.value, make_first: first.checked,
        limit: limit.value === initialLimit ? undefined : limit.value
      });
      dialog.close();
    } catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
    }
  };
}

function toolCreateBaseNote(path, view, refresh, open) {
  const { dialog, body, status } = toolDialog("New note · " + view.title);
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const name = toolNode("input");
  name.required = true;
  const nameLabel = toolNode("label", "Note name");
  nameLabel.append(name);
  const folder = toolNode("input");
  folder.value = view.new_note?.folder ?? path.slice(0, Math.max(0, path.lastIndexOf("/")));
  const folderLabel = toolNode("label", "Folder path within the vault (optional)");
  folderLabel.append(folder);
  const content = toolNode("textarea");
  content.rows = 6;
  content.value = view.new_note?.content ?? "";
  const contentLabel = toolNode("label", "Note content");
  contentLabel.append(content);
  const save = toolNode("button", "Create note");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  fields.append(nameLabel, folderLabel, contentLabel, save);
  form.append(fields);
  body.append(form);
  name.focus();
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    const stem = name.value.trim().replace(/\.md$/i, "");
    if (!stem || /[\\/]/.test(stem)) { status("Enter a file name without a folder path."); return; }
    const directory = folder.value.trim().replaceAll("\\", "/").replace(/\/+$/, "");
    if (directory && directory.split("/").some(part => !part || part === "." || part === "..")) {
      status("Enter a folder relative to the vault.");
      return;
    }
    const note = directory ? directory + "/" + stem : stem;
    fields.disabled = close.disabled = true;
    status("");
    try {
      await toolFlush();
      const result = await createNote(note, content.value, (_, error) => status(errStr(error)));
      if (result === "exists") status("A note with this name already exists.");
      if (result !== "ok") return;
      form.remove();
      const openNote = toolButton("Open note", () => { dialog.close(); open(note); });
      body.append(openNote);
      status("Created " + note + ".md");
      try {
        if (!await refresh(note)) status("Created " + note + ".md. This note is outside the current results.");
      } catch (error) {
        status("Created " + note + ".md. Could not refresh the table. " + errStr(error));
      }
      openNote.focus();
    } catch (error) { status(errStr(error)); }
    finally { fields.disabled = close.disabled = false; }
  };
}

function toolEditBaseFilters(base, view, write) {
  const { dialog, body, status } = toolDialog("Edit filters · " + (view?.title ?? "All views"));
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const stored = view ? view.filters : base.filters;
  let read;
  if (stored.mode === "yaml") {
    const input = toolNode("textarea");
    input.rows = 8;
    input.spellcheck = false;
    input.value = stored.yaml;
    const label = toolNode("label", "Filter YAML");
    label.append(input);
    fields.append(label, toolNode("p", view ? "Clear the YAML to remove the filters from this view."
      : "Clear the YAML to remove the base-wide filters."));
    read = () => ({ mode: "yaml", yaml: input.value });
  } else {
    const properties = view ? baseViewPropertyOptions(base, view)
      .filter(([key]) => key.startsWith("note.")).map(([key]) => key.slice(5)) : base.properties;
    const conditions = baseConditionFields(properties, stored.conditions);
    const folder = toolNode("input");
    folder.value = stored.folder ?? "";
    const folderLabel = toolNode("label", "Folder path within the vault (optional)");
    folderLabel.append(folder);
    const markdown = toolNode("input");
    markdown.type = "checkbox";
    markdown.checked = stored.markdown_only ?? false;
    const markdownLabel = toolNode("label");
    markdownLabel.append(markdown, document.createTextNode(" Restrict to Markdown files"));
    fields.append(conditions.filters, folderLabel, markdownLabel);
    read = () => ({
      mode: "conditions", conditions: conditions.read(),
      folder: folder.value || undefined, markdown_only: markdown.checked || undefined
    });
  }
  const save = toolNode("button", "Save filters");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  fields.append(save);
  form.append(fields);
  const scope = view ? "Base-wide filters still apply." : "These filters apply to every view in this base.";
  body.append(toolNode("p", scope + " Saving a change rewrites the YAML formatting and removes comments."), form);
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    fields.disabled = close.disabled = true;
    status("");
    try { await write(read()); dialog.close(); }
    catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
    }
  };
}

function toolEditBaseColumns(base, view, write) {
  const { dialog, body, status } = toolDialog("Edit columns · " + view.title);
  const options = new Map(baseViewPropertyOptions(base, view));
  const columns = [...view.columns];
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const shown = toolSelect("Visible columns in display order", []);
  shown.size = 6;
  const shownLabel = toolNode("label", "Visible columns");
  shownLabel.append(shown);
  const available = toolSelect("Available columns", []);
  const add = toolButton("Add column", () => {
    if (!available.value || columns.length >= 128) return;
    columns.push(available.value);
    refresh(columns.length - 1);
    shown.focus();
  });
  const move = offset => {
    const index = Number(shown.value);
    const next = index + offset;
    [columns[index], columns[next]] = [columns[next], columns[index]];
    refresh(next);
    shown.focus();
  };
  const up = toolButton("Move up", () => move(-1));
  const down = toolButton("Move down", () => move(1));
  const remove = toolButton("Hide column", () => {
    const index = Number(shown.value);
    columns.splice(index, 1);
    refresh(Math.min(index, columns.length - 1));
    shown.focus();
  });
  const option = (value, name) => Object.assign(toolNode("option", name), { value });
  const updateControls = () => {
    const index = Number(shown.value);
    up.disabled = index === 0;
    down.disabled = index === columns.length - 1;
    remove.disabled = columns.length === 1;
    add.disabled = !available.value || columns.length >= 128;
  };
  function refresh(index) {
    shown.replaceChildren(...columns.map((key, i) => option(String(i), options.get(key))));
    shown.value = String(index);
    const used = new Set(columns);
    available.replaceChildren(option("", "Choose a property"),
      ...[...options].filter(([key]) => !used.has(key)).map(([key, name]) => option(key, name)));
    available.value = "";
    updateControls();
  }
  shown.onchange = available.onchange = updateControls;
  refresh(0);
  const save = toolNode("button", "Save columns");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  const picker = toolNode("div");
  picker.append(available, add);
  fields.append(shownLabel, up, down, remove, picker, save);
  form.append(fields);
  body.append(toolNode("p", "Saving a change rewrites the YAML formatting and removes comments."), form);
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    fields.disabled = close.disabled = true;
    status("");
    try { await write(columns); dialog.close(); }
    catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
    }
  };
}

function toolEditBaseSorting(base, view, write) {
  const { dialog, body, status } = toolDialog("Edit sorting · " + view.title);
  const options = baseViewPropertyOptions(base, view);
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const rules = toolNode("div");
  const controls = [];
  const read = () => controls.map(({ property, direction }) => ({ property: property.value, direction: direction.value }));
  const add = toolButton("Add sort rule", () => {
    const used = new Set(read().map(rule => rule.property));
    const next = options.find(([key]) => !used.has(key));
    if (next) appendRule({ property: next[0], direction: "ASC" }).focus();
  });
  const updateAdd = () => {
    add.disabled = new Set(read().map(rule => rule.property)).size >= options.length;
  };
  const refresh = () => {
    rules.replaceChildren(...controls.map(control => control.row));
    controls.forEach((control, index) => {
      control.up.disabled = index === 0;
      control.down.disabled = index === controls.length - 1;
    });
    updateAdd();
  };
  const move = (control, offset) => {
    const index = controls.indexOf(control);
    controls.splice(index, 1);
    controls.splice(index + offset, 0, control);
    refresh();
    control.property.focus();
  };
  function appendRule(rule) {
    const row = toolNode("div");
    const property = toolSelect("Sort property", options);
    property.value = rule.property;
    property.onchange = updateAdd;
    const direction = toolSelect("Sort direction", [["ASC", "Ascending"], ["DESC", "Descending"]]);
    direction.value = rule.direction;
    const up = toolButton("Move up", () => move(control, -1));
    const down = toolButton("Move down", () => move(control, 1));
    const remove = toolButton("Remove", () => {
      const index = controls.indexOf(control);
      controls.splice(index, 1);
      refresh();
      (controls[index]?.property ?? controls.at(-1)?.property ?? add).focus();
    });
    const control = { row, property, direction, up, down };
    row.append(property, direction, up, down, remove);
    controls.push(control);
    refresh();
    return property;
  }
  view.sort.forEach(appendRule);
  const save = toolNode("button", "Save sorting");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  fields.append(toolNode("legend", "Sort rules in priority order"), rules, add, save);
  form.append(fields);
  body.append(toolNode("p", "Saving a change rewrites the YAML formatting and removes comments."), form);
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    const sort = read();
    if (new Set(sort.map(rule => rule.property)).size !== sort.length) { status("Choose a different property for each rule."); return; }
    fields.disabled = close.disabled = true;
    status("");
    try { await write(sort); dialog.close(); }
    catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
    }
  };
}

function toolEditBaseGrouping(base, view, write) {
  const { dialog, body, status } = toolDialog("Edit grouping · " + view.title);
  const grouping = baseGroupingFields(baseViewPropertyOptions(base, view), view.group_by, view.manual_groups);
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const save = toolNode("button", "Save grouping");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  fields.append(grouping.label, grouping.groupDirection, save);
  form.append(fields);
  body.append(toolNode("p", "Saving a change rewrites the YAML formatting and removes comments."), form);
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    const group = grouping.read();
    if (group?.direction === "manual") { dialog.close(); return; }
    fields.disabled = close.disabled = true;
    status("");
    try { await write(group); dialog.close(); }
    catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
    }
  };
}

function toolExportBaseCsv(path, headers, rows) {
  const { dialog, body, status } = toolDialog("Export table as CSV");
  const form = toolNode("form");
  const fields = toolNode("fieldset");
  const name = toolNode("input");
  name.value = path.split("/").pop().replace(/\.base$/i, "") + ".csv";
  name.required = true;
  const nameLabel = toolNode("label", "File name");
  nameLabel.append(name);
  const separator = toolSelect("Separator", [["comma", "Comma"], ["semicolon", "Semicolon"]]);
  const separatorLabel = toolNode("label", "Separator");
  separatorLabel.append(separator);
  const save = toolNode("button", "Save CSV");
  save.type = "submit";
  const close = dialog.querySelector("header button");
  fields.append(nameLabel, separatorLabel, save);
  form.append(fields);
  body.append(toolNode("p", "The CSV is saved next to the base in this vault."), form);
  name.focus();
  dialog.addEventListener("cancel", event => { if (fields.disabled) event.preventDefault(); });
  form.onsubmit = async event => {
    event.preventDefault();
    if (fields.disabled) return;
    const stem = name.value.trim().replace(/\.csv$/i, "");
    if (!stem || /[\\/]/.test(stem)) { status("Enter a file name without a folder path."); return; }
    fields.disabled = close.disabled = true;
    status("");
    try {
      const saved = await inv("export_vault_base_csv", { path, filename: stem + ".csv", headers, rows, separator: separator.value });
      form.remove();
      status("Saved " + saved);
      close.disabled = false;
      const reveal = toolButton("Show in Explorer", async () => {
        try { await inv("vault_file_action", { path: saved, action: "reveal" }); }
        catch (error) { status(errStr(error)); }
      });
      body.append(reveal);
      reveal.focus();
    } catch (error) {
      status(errStr(error));
      fields.disabled = close.disabled = false;
    }
  };
}

async function toolOpenBase(path) {
  const { dialog, body, status } = toolDialog(path);
  try {
    await toolFlush();
    let base = await inv("open_vault_base", { path });
    if (!dialog.open) return;
    const choose = toolSelect("Table view", []);
    const refreshViewPicker = (selection) => {
      choose.replaceChildren(...base.views.map((view, index) => {
        const option = toolNode("option", view.title);
        option.value = String(index);
        return option;
      }));
      choose.value = String(selection);
      choose.hidden = base.views.length === 1;
    };
    refreshViewPicker(0);
    const filter = toolFilter("Filter this table");
    const table = toolNode("table");
    const head = table.createTHead().insertRow();
    const collapsed = new Set();
    const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
    const activeView = () => base.views[Number(choose.value)];
    const defaultSort = view => view.sort.length ? view.sort : [{ property: view.columns[0], direction: "ASC" }];
    let sortOverride = null;
    let results = [];
    const newNote = toolButton("New note", () => {
      const view = activeView();
      toolCreateBaseNote(path, view, async name => {
        base = await inv("open_vault_base", { path });
        refreshViewPicker(Math.max(0, base.views.findIndex(candidate => candidate.title === view.title)));
        collapsed.clear();
        render();
        return results.some(row => row.name === name);
      }, name => { dialog.close(); void openInTab(name); });
    });
    const addView = toolButton("Add view", () => {
      toolAddBaseView(base, activeView(), async config => {
        base = await inv("add_vault_base_view", { path, revision: base.revision, config });
        refreshViewPicker(base.views.length - 1);
        filter.value = "";
        sortOverride = null;
        collapsed.clear();
        render();
      });
    });
    const editView = toolButton("Edit view", () => {
      const selection = Number(choose.value);
      const view = activeView();
      toolEditBaseView(view, async config => {
        base = await inv("set_vault_base_view", { path, revision: base.revision, id: view.id, config });
        refreshViewPicker(config.make_first ? 0 : selection);
        collapsed.clear();
        render();
      });
    });
    const edit = toolButton("Edit grouping", () => {
      const view = activeView();
      toolEditBaseGrouping(base, view, async groupBy => {
        base = await inv("set_vault_base_grouping", { path, revision: base.revision, id: view.id, groupBy });
        collapsed.clear();
        render();
      });
    });
    const editSort = toolButton("Edit sorting", () => {
      const view = activeView();
      toolEditBaseSorting(base, view, async rules => {
        base = await inv("set_vault_base_sorting", { path, revision: base.revision, id: view.id, sort: rules });
        sortOverride = null;
        collapsed.clear();
        render();
      });
    });
    const editColumns = toolButton("Edit columns", () => {
      const view = activeView();
      toolEditBaseColumns(base, view, async columns => {
        base = await inv("set_vault_base_columns", { path, revision: base.revision, id: view.id, columns });
        collapsed.clear();
        render();
      });
    });
    const editFilters = toolButton("Edit filters", () => {
      const view = activeView();
      toolEditBaseFilters(base, view, async filters => {
        base = await inv("set_vault_base_filters", { path, revision: base.revision, id: view.id, filters });
        collapsed.clear();
        render();
      });
    });
    const editBaseFilters = toolButton("Edit base filters", () => {
      toolEditBaseFilters(base, null, async filters => {
        base = await inv("set_vault_base_global_filters", { path, revision: base.revision, filters });
        collapsed.clear();
        render();
      });
    });
    const value = (row, column) => {
      if (column === "file.name") return row.name.split("/").pop();
      if (column === "file.path") return row.name + ".md";
      if (column === "file.folder") return row.name.slice(0, Math.max(0, row.name.lastIndexOf("/")));
      if (column === "file.ext") return "md";
      const key = column.slice("note.".length);
      return Object.hasOwn(row.values, key) ? row.values[key] : "";
    };
    const label = (view, key) => view.labels?.[key] ?? baseFileProperties.find(([property]) => key === property)?.[1] ?? key.slice(5);
    const tableResults = () => {
      const view = activeView();
      return {
        headers: view.columns.map(key => label(view, key)),
        rows: results.map(row => view.columns.map(key => value(row, key)))
      };
    };
    const exportCsv = toolButton("Export CSV", () => {
      const { headers, rows } = tableResults();
      toolExportBaseCsv(path, headers, rows);
    });
    const copyTable = toolButton("Copy table", async () => {
      const snapshot = results;
      copyTable.disabled = true;
      try {
        await inv("copy_vault_base_table", tableResults());
        if (dialog.open && results === snapshot) status(snapshot.length ? "Table copied" : "Column headers copied");
      } catch (error) {
        if (dialog.open) status(errStr(error));
      } finally {
        copyTable.disabled = false;
      }
    });
    const render = () => {
      const view = activeView();
      addView.disabled = !base.can_add_view;
      addView.title = addView.disabled ? "This base already has sixteen views." : "";
      const sort = sortOverride ?? defaultSort(view);
      const query = filter.value.trim().toLocaleLowerCase();
      results = [];
      head.replaceChildren();
      for (const key of view.columns) {
        const cell = toolNode("th");
        cell.scope = "col";
        const primary = sort[0];
        cell.setAttribute("aria-sort", key === primary.property ? (primary.direction === "DESC" ? "descending" : "ascending") : "none");
        cell.append(toolButton(label(view, key), () => {
          const direction = primary.property === key && primary.direction === "ASC" ? "DESC" : "ASC";
          sortOverride = [{ property: key, direction }, ...sort.filter(rule => rule.property !== key)];
          render();
        }));
        head.append(cell);
      }
      while (table.tBodies.length) table.tBodies[0].remove();
      for (const group of view.groups ?? [{ rows: view.rows }]) {
        const matches = group.rows.filter(row => view.columns.some(key => value(row, key).toLocaleLowerCase().includes(query)));
        matches.sort((a, b) => {
          for (const { property, direction } of sort) {
            const order = collator.compare(value(a, property), value(b, property));
            if (order) return direction === "DESC" ? -order : order;
          }
          return collator.compare(a.name, b.name);
        });
        const visible = matches.slice(0, (view.limit ?? Infinity) - results.length);
        if (!visible.length && (query || group.rows.length)) continue;
        const data = table.createTBody();
        const items = [];
        if (view.groups) {
          const heading = toolNode("th");
          heading.scope = "rowgroup";
          heading.colSpan = view.columns.length;
          const label = `${group.label} (${visible.length})`;
          if (visible.length) {
            const toggle = toolButton(label, () => {
              if (!collapsed.delete(group)) collapsed.add(group);
              const expanded = !collapsed.has(group);
              toggle.setAttribute("aria-expanded", String(expanded));
              for (const item of items) item.hidden = !expanded;
            });
            toggle.setAttribute("aria-expanded", String(!collapsed.has(group)));
            heading.append(toggle);
          } else heading.textContent = label;
          data.insertRow().append(heading);
        }
        for (const row of visible) {
          results.push(row);
          const item = data.insertRow();
          item.hidden = collapsed.has(group);
          items.push(item);
          view.columns.forEach((key, index) => {
            const cell = item.insertCell();
            const text = value(row, key);
            if (index === 0) {
              const link = toolButton(text || row.name, () => { dialog.close(); void openInTab(row.name); });
              link.title = row.name;
              cell.append(link);
            } else cell.textContent = text;
          });
        }
      }
      status(results.length ? `${results.length} notes` : "No notes match.");
    };
    choose.onchange = () => {
      sortOverride = null;
      render();
    };
    filter.oninput = render;
    body.append(filter, choose, addView, editView, edit, editSort, editColumns, editFilters, editBaseFilters, exportCsv, copyTable, newNote, table);
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
