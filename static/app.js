(function () {
  'use strict';

  const API = '/api/tasks';
  const statusIcons = { todo: '\u25CB', in_progress: '\u25D0', in_review: '\u25D1', done: '\u25CF', blocked: '\u2715' };
  const statusOrder = ['todo', 'in_progress', 'in_review', 'done', 'blocked'];
  const priorityLabels = { low: 'low', medium: 'med', high: 'high', critical: 'crit' };

  let tasks = [];
  let collapsed = new Set(JSON.parse(localStorage.getItem('collapsed') || '[]'));

  // ── API ───────────────────────────────────────────────────────────────

  async function fetchTasks() {
    const res = await fetch(API);
    tasks = await res.json();
    render();
  }

  async function addTask(title, priority, parentId) {
    const body = { title, priority };
    if (parentId != null) body.parent_id = parentId;
    await fetch(API, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    await fetchTasks();
  }

  async function cycleStatus(id, currentStatus) {
    const idx = statusOrder.indexOf(currentStatus);
    const next = statusOrder[(idx + 1) % 4]; // cycle todo → in_progress → in_review → done → todo (skip blocked)
    await fetch(`${API}/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status: next }),
    });
    await fetchTasks();
  }

  async function deleteTask(id) {
    await fetch(`${API}/${id}`, { method: 'DELETE' });
    await fetchTasks();
  }

  // ── Tree logic ────────────────────────────────────────────────────────

  function buildTree() {
    const childrenMap = new Map();
    const roots = [];
    for (const t of tasks) {
      if (!childrenMap.has(t.id)) childrenMap.set(t.id, []);
      if (t.parent_id != null) {
        if (!childrenMap.has(t.parent_id)) childrenMap.set(t.parent_id, []);
        childrenMap.get(t.parent_id).push(t);
      } else {
        roots.push(t);
      }
    }
    // Flatten via depth-first walk
    const rows = [];
    function walk(node, depth) {
      const children = childrenMap.get(node.id) || [];
      rows.push({ task: node, depth, hasChildren: children.length > 0 });
      if (!collapsed.has(node.id)) {
        for (const child of children) walk(child, depth + 1);
      }
    }
    for (const root of roots) walk(root, 0);
    return rows;
  }

  // ── Rendering ─────────────────────────────────────────────────────────

  function render() {
    const container = document.getElementById('task-list');
    const rows = buildTree();

    if (rows.length === 0) {
      container.innerHTML = '<div class="empty-state">No tasks yet. Add one above.</div>';
      return;
    }

    container.innerHTML = '';
    for (const { task, depth, hasChildren } of rows) {
      container.appendChild(createTaskRow(task, depth, hasChildren));
    }
  }

  function createTaskRow(task, depth, hasChildren) {
    const row = document.createElement('div');
    row.className = 'task-row';

    // Indentation
    const indent = document.createElement('span');
    indent.className = 'task-indent';
    indent.style.width = (depth * 24) + 'px';
    row.appendChild(indent);

    // Expand/collapse toggle
    const expand = document.createElement('span');
    expand.className = 'task-expand';
    if (hasChildren) {
      expand.textContent = collapsed.has(task.id) ? '\u25B6' : '\u25BC';
      expand.addEventListener('click', () => {
        if (collapsed.has(task.id)) collapsed.delete(task.id);
        else collapsed.add(task.id);
        localStorage.setItem('collapsed', JSON.stringify([...collapsed]));
        render();
      });
    }
    row.appendChild(expand);

    // Status icon
    const status = document.createElement('span');
    status.className = `task-status status-${task.status}`;
    status.textContent = statusIcons[task.status] || '\u25CB';
    status.title = 'Click to cycle status';
    status.addEventListener('click', () => cycleStatus(task.id, task.status));
    row.appendChild(status);

    // Title
    const title = document.createElement('span');
    title.className = 'task-title' + (task.status === 'done' ? ' title-done' : '');
    title.textContent = task.title;
    row.appendChild(title);

    // Tags
    if (task.tags && task.tags.length > 0 && !(task.tags.length === 1 && task.tags[0] === '')) {
      const tags = document.createElement('span');
      tags.className = 'task-tags';
      tags.textContent = task.tags.map(t => '#' + t).join(' ');
      row.appendChild(tags);
    }

    // Priority badge
    const priority = document.createElement('span');
    priority.className = `task-priority priority-${task.priority}`;
    priority.textContent = priorityLabels[task.priority] || task.priority;
    row.appendChild(priority);

    // Actions
    const actions = document.createElement('span');
    actions.className = 'task-actions';

    const addBtn = document.createElement('button');
    addBtn.textContent = '+ sub';
    addBtn.addEventListener('click', () => showSubtaskInput(row, task.id));
    actions.appendChild(addBtn);

    const delBtn = document.createElement('button');
    delBtn.className = 'delete-btn';
    delBtn.textContent = 'del';
    delBtn.addEventListener('click', () => {
      if (confirm(`Delete "${task.title}"?`)) deleteTask(task.id);
    });
    actions.appendChild(delBtn);

    row.appendChild(actions);
    return row;
  }

  function showSubtaskInput(afterRow, parentId) {
    // Don't add duplicate input
    if (afterRow.nextSibling && afterRow.nextSibling.classList &&
        afterRow.nextSibling.classList.contains('subtask-form')) return;

    const form = document.createElement('div');
    form.className = 'subtask-form';

    // Match parent indentation + one level
    const depth = parseInt(afterRow.querySelector('.task-indent').style.width) || 0;
    const indent = document.createElement('span');
    indent.style.width = (depth + 24 + 18 + 10) + 'px'; // indent + expand + gap
    indent.style.flexShrink = '0';
    form.appendChild(indent);

    const input = document.createElement('input');
    input.type = 'text';
    input.placeholder = 'Subtask title...';
    form.appendChild(input);

    afterRow.parentNode.insertBefore(form, afterRow.nextSibling);
    input.focus();

    function submit() {
      const val = input.value.trim();
      if (val) {
        addTask(val, 'medium', parentId);
        // Ensure parent is expanded
        collapsed.delete(parentId);
        localStorage.setItem('collapsed', JSON.stringify([...collapsed]));
      }
      form.remove();
    }

    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') submit();
      if (e.key === 'Escape') form.remove();
    });
    input.addEventListener('blur', submit);
  }

  // ── Init ──────────────────────────────────────────────────────────────

  document.getElementById('add-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const input = document.getElementById('add-input');
    const priority = document.getElementById('add-priority').value;
    const title = input.value.trim();
    if (!title) return;
    input.value = '';
    addTask(title, priority, null);
  });

  fetchTasks();
})();
