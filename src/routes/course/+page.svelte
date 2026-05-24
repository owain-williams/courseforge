<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getWindowCourseFolder,
    readCourse,
    addModule,
    renameModule,
    reorderModules,
    deleteModule,
    addVideo,
    renameVideo,
    reorderVideosInModule,
    deleteVideo,
    moveVideoToModule,
    addWorkflowState,
    renameWorkflowState,
    reorderWorkflowStates,
    removeWorkflowState,
    setVideoState,
    type Course,
    type Module,
    type Video,
    type WorkflowState
  } from '$lib/api';

  let folder = $state<string | null>(null);
  let course = $state<Course | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  // Tree vs Board.
  let view = $state<'tree' | 'board'>('tree');

  // Workflow state editor (modal-ish inline panel).
  let editingStates = $state(false);
  let newStateDraft = $state('');
  let editingStateId = $state<string | null>(null);
  let editStateDraft = $state('');

  // Drag state for the board view. Tracked at module level so we can
  // highlight the drop target column.
  let draggingVideoId = $state<string | null>(null);
  let dropTargetStateId = $state<string | null>(null);

  // Inline edit state
  let editing = $state<{ kind: 'module' | 'video'; id: string } | null>(null);
  let editDraft = $state('');
  let editInputEl = $state<HTMLInputElement | null>(null);

  // New-row state per module
  let newVideoModuleId = $state<string | null>(null);
  let newVideoDraft = $state('');
  let newVideoInputEl = $state<HTMLInputElement | null>(null);

  // New module draft
  let newModuleOpen = $state(false);
  let newModuleDraft = $state('');
  let newModuleInputEl = $state<HTMLInputElement | null>(null);

  $effect(() => {
    if (editing && editInputEl) {
      editInputEl.focus();
      editInputEl.select();
    }
  });
  $effect(() => {
    if (newVideoModuleId && newVideoInputEl) {
      newVideoInputEl.focus();
      newVideoInputEl.select();
    }
  });
  $effect(() => {
    if (newModuleOpen && newModuleInputEl) {
      newModuleInputEl.focus();
      newModuleInputEl.select();
    }
  });

  async function refresh() {
    if (!folder) return;
    try {
      course = await readCourse(folder);
    } catch (e) {
      error = String(e);
    }
  }

  function videoById(id: string): Video | undefined {
    return course?.videos.find((v) => v.id === id);
  }

  async function withBusy<T>(fn: () => Promise<T>): Promise<T | undefined> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      return await fn();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function openNewModule() {
    newModuleDraft = 'New Module';
    newModuleOpen = true;
  }
  function cancelNewModule() {
    newModuleOpen = false;
    newModuleDraft = '';
  }
  async function commitNewModule() {
    const title = newModuleDraft.trim();
    if (!folder || !title) {
      cancelNewModule();
      return;
    }
    await withBusy(async () => {
      await addModule(folder!, title);
      newModuleOpen = false;
      newModuleDraft = '';
      await refresh();
    });
  }

  function startEdit(kind: 'module' | 'video', id: string, currentTitle: string) {
    editing = { kind, id };
    editDraft = currentTitle;
  }
  function cancelEdit() {
    editing = null;
    editDraft = '';
  }
  async function commitEdit() {
    if (!editing || !folder) return cancelEdit();
    const next = editDraft.trim();
    const { kind, id } = editing;
    const currentTitle =
      kind === 'module'
        ? course?.modules.find((m) => m.id === id)?.title
        : videoById(id)?.title;
    if (!next || next === currentTitle) return cancelEdit();
    await withBusy(async () => {
      if (kind === 'module') await renameModule(folder!, id, next);
      else await renameVideo(folder!, id, next);
      editing = null;
      await refresh();
    });
  }

  async function deleteModuleAt(m: Module) {
    if (!folder) return;
    const childCount = m.videoIds.length;
    const msg =
      childCount === 0
        ? `Delete module "${m.title}"?`
        : `Delete module "${m.title}" and its ${childCount} video${childCount === 1 ? '' : 's'}?`;
    if (!confirm(msg)) return;
    await withBusy(async () => {
      await deleteModule(folder!, m.id);
      await refresh();
    });
  }

  async function deleteVideoAt(v: Video) {
    if (!folder) return;
    if (!confirm(`Delete video "${v.title}"?`)) return;
    await withBusy(async () => {
      await deleteVideo(folder!, v.id);
      await refresh();
    });
  }

  function openNewVideoForm(moduleId: string) {
    newVideoModuleId = moduleId;
    newVideoDraft = 'New Video';
  }
  function cancelNewVideo() {
    newVideoModuleId = null;
    newVideoDraft = '';
  }
  async function commitNewVideo() {
    const title = newVideoDraft.trim();
    const mid = newVideoModuleId;
    if (!folder || !mid || !title) return cancelNewVideo();
    await withBusy(async () => {
      await addVideo(folder!, mid, title);
      newVideoModuleId = null;
      newVideoDraft = '';
      await refresh();
    });
  }

  async function moveModule(m: Module, delta: -1 | 1) {
    if (!course || !folder) return;
    const ids = course.modules.map((x) => x.id);
    const i = ids.indexOf(m.id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    await withBusy(async () => {
      await reorderModules(folder!, ids);
      await refresh();
    });
  }

  async function moveVideoInModule(m: Module, v: Video, delta: -1 | 1) {
    if (!folder) return;
    const ids = [...m.videoIds];
    const i = ids.indexOf(v.id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    await withBusy(async () => {
      await reorderVideosInModule(folder!, m.id, ids);
      await refresh();
    });
  }

  async function moveVideoToOtherModule(v: Video, targetModuleId: string) {
    if (!folder) return;
    await withBusy(async () => {
      const dst = course?.modules.find((m) => m.id === targetModuleId);
      const index = dst ? dst.videoIds.length : 0;
      await moveVideoToModule(folder!, v.id, targetModuleId, index);
      await refresh();
    });
  }

  function stateById(id: string | null): WorkflowState | undefined {
    if (!id) return undefined;
    return course?.workflowStates.find((s) => s.id === id);
  }

  function videosInState(stateId: string): Video[] {
    if (!course) return [];
    return course.videos.filter((v) => (v.stateId ?? course!.workflowStates[0]?.id) === stateId);
  }

  function moduleTitleForVideo(videoId: string): string | undefined {
    return course?.modules.find((m) => m.videoIds.includes(videoId))?.title;
  }

  async function transitionVideo(videoId: string, newStateId: string) {
    if (!folder) return;
    const v = course?.videos.find((x) => x.id === videoId);
    if (!v || v.stateId === newStateId) return;
    await withBusy(async () => {
      await setVideoState(folder!, videoId, newStateId);
      await refresh();
    });
  }

  // Board drag handlers — HTML5 native DnD, no extra deps.
  function onCardDragStart(e: DragEvent, videoId: string) {
    draggingVideoId = videoId;
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move';
      // Some browsers require any data to be set for the drag to fire.
      e.dataTransfer.setData('text/plain', videoId);
    }
  }
  function onCardDragEnd() {
    draggingVideoId = null;
    dropTargetStateId = null;
  }
  function onColumnDragOver(e: DragEvent, stateId: string) {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    dropTargetStateId = stateId;
  }
  function onColumnDragLeave(stateId: string) {
    if (dropTargetStateId === stateId) dropTargetStateId = null;
  }
  async function onColumnDrop(e: DragEvent, stateId: string) {
    e.preventDefault();
    const id = draggingVideoId ?? e.dataTransfer?.getData('text/plain') ?? '';
    draggingVideoId = null;
    dropTargetStateId = null;
    if (id) await transitionVideo(id, stateId);
  }

  // ----- Workflow state editor handlers -----
  function openStateEditor() {
    editingStates = true;
    newStateDraft = '';
    editingStateId = null;
    editStateDraft = '';
  }
  function closeStateEditor() {
    editingStates = false;
    editingStateId = null;
    editStateDraft = '';
    newStateDraft = '';
  }
  async function commitAddState() {
    if (!folder) return;
    const name = newStateDraft.trim();
    if (!name) return;
    await withBusy(async () => {
      await addWorkflowState(folder!, name);
      newStateDraft = '';
      await refresh();
    });
  }
  function startEditState(s: WorkflowState) {
    editingStateId = s.id;
    editStateDraft = s.name;
  }
  function cancelEditState() {
    editingStateId = null;
    editStateDraft = '';
  }
  async function commitEditState() {
    if (!folder || !editingStateId) return cancelEditState();
    const next = editStateDraft.trim();
    const current = course?.workflowStates.find((s) => s.id === editingStateId)?.name;
    if (!next || next === current) return cancelEditState();
    const id = editingStateId;
    await withBusy(async () => {
      await renameWorkflowState(folder!, id, next);
      editingStateId = null;
      await refresh();
    });
  }
  async function moveStateOrder(s: WorkflowState, delta: -1 | 1) {
    if (!course || !folder) return;
    const ids = course.workflowStates.map((x) => x.id);
    const i = ids.indexOf(s.id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    await withBusy(async () => {
      await reorderWorkflowStates(folder!, ids);
      await refresh();
    });
  }
  async function removeStateAt(s: WorkflowState) {
    if (!course || !folder) return;
    if (course.workflowStates.length <= 1) {
      alert('Cannot remove the last remaining workflow state.');
      return;
    }
    const affected = course.videos.filter((v) => v.stateId === s.id).length;
    const choices = course.workflowStates.filter((x) => x.id !== s.id);
    const promptMsg =
      affected === 0
        ? `Remove state "${s.name}"?\n\nNo videos are currently in this state.\n\nEnter the name of a state to move (irrelevant — leave default), then OK:`
        : `Remove state "${s.name}"?\n\n${affected} video${affected === 1 ? '' : 's'} will be moved to a state of your choice.\n\nEnter the name of the destination state:\n  ${choices.map((c) => c.name).join('\n  ')}`;
    const fallbackName = window.prompt(promptMsg, choices[0]?.name ?? '');
    if (fallbackName == null) return;
    const fallback = choices.find(
      (c) => c.name.toLowerCase() === fallbackName.trim().toLowerCase()
    );
    if (!fallback) {
      alert(`No state named "${fallbackName}". Aborted.`);
      return;
    }
    await withBusy(async () => {
      await removeWorkflowState(folder!, s.id, fallback.id);
      await refresh();
    });
  }

  onMount(() => {
    void (async () => {
      try {
        folder = await getWindowCourseFolder();
        if (!folder) {
          error = 'This window is not bound to a Course folder.';
          return;
        }
        await refresh();
      } catch (e) {
        error = String(e);
      }
    })();
  });
</script>

<main>
  <header>
    <h1>{course?.title ?? 'Loading…'}</h1>
    {#if folder}
      <code class="folder" title={folder}>{folder}</code>
    {/if}
  </header>

  {#if error}
    <div class="error" role="alert">{error}</div>
  {/if}

  {#if course}
    <section class="view-toolbar">
      <div class="view-toggle" role="tablist" aria-label="View">
        <button
          role="tab"
          aria-selected={view === 'tree'}
          class:active={view === 'tree'}
          onclick={() => (view = 'tree')}
        >Tree</button>
        <button
          role="tab"
          aria-selected={view === 'board'}
          class:active={view === 'board'}
          onclick={() => (view = 'board')}
        >Board</button>
      </div>
      <button class="ghost" onclick={openStateEditor} disabled={busy}>Edit States…</button>
    </section>

    {#if editingStates}
      <section class="state-editor" aria-label="Workflow states">
        <header>
          <h2>Workflow States</h2>
          <button class="ghost" onclick={closeStateEditor} disabled={busy}>Done</button>
        </header>
        <ol class="states">
          {#each course.workflowStates as s, si (s.id)}
            <li class="state-row">
              <div class="title-area">
                {#if editingStateId === s.id}
                  <input
                    bind:value={editStateDraft}
                    onblur={commitEditState}
                    onkeydown={(e) => {
                      if (e.key === 'Enter') commitEditState();
                      else if (e.key === 'Escape') cancelEditState();
                    }}
                  />
                {:else}
                  <button class="title-btn" onclick={() => startEditState(s)}>{s.name}</button>
                {/if}
              </div>
              <div class="actions">
                <button class="icon" disabled={si === 0 || busy} onclick={() => moveStateOrder(s, -1)} title="Move up">↑</button>
                <button class="icon" disabled={si === course.workflowStates.length - 1 || busy} onclick={() => moveStateOrder(s, 1)} title="Move down">↓</button>
                <button class="link" disabled={busy} onclick={() => startEditState(s)}>Rename</button>
                <button
                  class="link danger"
                  disabled={busy || course.workflowStates.length <= 1}
                  onclick={() => removeStateAt(s)}
                >Remove</button>
              </div>
            </li>
          {/each}
        </ol>
        <form
          class="new-form"
          onsubmit={(e) => {
            e.preventDefault();
            void commitAddState();
          }}
        >
          <input
            bind:value={newStateDraft}
            placeholder="New state name"
          />
          <button class="primary" type="submit" disabled={busy || !newStateDraft.trim()}>
            Add State
          </button>
        </form>
      </section>
    {/if}

    {#if view === 'tree'}
    <section class="toolbar">
      {#if newModuleOpen}
        <form
          class="new-form"
          onsubmit={(e) => {
            e.preventDefault();
            void commitNewModule();
          }}
        >
          <input
            bind:this={newModuleInputEl}
            bind:value={newModuleDraft}
            placeholder="Module title"
            onkeydown={(e) => {
              if (e.key === 'Escape') cancelNewModule();
            }}
          />
          <button class="primary" type="submit" disabled={busy || !newModuleDraft.trim()}>
            Add
          </button>
          <button type="button" class="ghost" onclick={cancelNewModule} disabled={busy}>
            Cancel
          </button>
        </form>
      {:else}
        <button class="primary" onclick={openNewModule} disabled={busy}>+ Add Module</button>
      {/if}
    </section>

    {#if course.modules.length === 0}
      <section class="empty">
        <p>No Modules yet. Click <strong>Add Module</strong> to create your first one.</p>
      </section>
    {:else}
      <ol class="modules">
        {#each course.modules as m, mi (m.id)}
          <li class="module">
            <header class="module-header">
              <div class="title-area">
                {#if editing?.kind === 'module' && editing.id === m.id}
                  <input
                    bind:this={editInputEl}
                    bind:value={editDraft}
                    onblur={commitEdit}
                    onkeydown={(e) => {
                      if (e.key === 'Enter') commitEdit();
                      else if (e.key === 'Escape') cancelEdit();
                    }}
                  />
                {:else}
                  <button class="title-btn module-title" onclick={() => startEdit('module', m.id, m.title)}>
                    {m.title}
                  </button>
                {/if}
                <span class="count">{m.videoIds.length} video{m.videoIds.length === 1 ? '' : 's'}</span>
              </div>
              <div class="actions">
                <button class="icon" disabled={mi === 0 || busy} onclick={() => moveModule(m, -1)} title="Move up">↑</button>
                <button class="icon" disabled={mi === course.modules.length - 1 || busy} onclick={() => moveModule(m, 1)} title="Move down">↓</button>
                <button class="link" disabled={busy} onclick={() => startEdit('module', m.id, m.title)}>Rename</button>
                <button class="link danger" disabled={busy} onclick={() => deleteModuleAt(m)}>Delete</button>
              </div>
            </header>

            <ul class="videos">
              {#each m.videoIds as vid, vi (vid)}
                {@const v = videoById(vid)}
                {#if v}
                  <li class="video">
                    <div class="title-area">
                      {#if editing?.kind === 'video' && editing.id === v.id}
                        <input
                          bind:this={editInputEl}
                          bind:value={editDraft}
                          onblur={commitEdit}
                          onkeydown={(e) => {
                            if (e.key === 'Enter') commitEdit();
                            else if (e.key === 'Escape') cancelEdit();
                          }}
                        />
                      {:else}
                        <button class="title-btn" onclick={() => startEdit('video', v.id, v.title)}>
                          {v.title}
                        </button>
                      {/if}
                      {#if stateById(v.stateId)}
                        <span class="state-badge" title="Workflow state">
                          {stateById(v.stateId)?.name}
                        </span>
                      {/if}
                    </div>
                    <div class="actions">
                      <button class="icon" disabled={vi === 0 || busy} onclick={() => moveVideoInModule(m, v, -1)} title="Move up">↑</button>
                      <button class="icon" disabled={vi === m.videoIds.length - 1 || busy} onclick={() => moveVideoInModule(m, v, 1)} title="Move down">↓</button>
                      {#if course.modules.length > 1}
                        <select
                          disabled={busy}
                          value=""
                          onchange={(e) => {
                            const target = (e.currentTarget as HTMLSelectElement).value;
                            (e.currentTarget as HTMLSelectElement).value = '';
                            if (target) void moveVideoToOtherModule(v, target);
                          }}
                          title="Move to module"
                        >
                          <option value="">Move to…</option>
                          {#each course.modules as om (om.id)}
                            {#if om.id !== m.id}
                              <option value={om.id}>{om.title}</option>
                            {/if}
                          {/each}
                        </select>
                      {/if}
                      <button class="link" disabled={busy} onclick={() => startEdit('video', v.id, v.title)}>Rename</button>
                      <button class="link danger" disabled={busy} onclick={() => deleteVideoAt(v)}>Delete</button>
                    </div>
                  </li>
                {/if}
              {/each}

              <li class="add-video">
                {#if newVideoModuleId === m.id}
                  <form
                    class="new-form"
                    onsubmit={(e) => {
                      e.preventDefault();
                      void commitNewVideo();
                    }}
                  >
                    <input
                      bind:this={newVideoInputEl}
                      bind:value={newVideoDraft}
                      placeholder="Video title"
                      onkeydown={(e) => {
                        if (e.key === 'Escape') cancelNewVideo();
                      }}
                    />
                    <button class="primary" type="submit" disabled={busy || !newVideoDraft.trim()}>
                      Add
                    </button>
                    <button type="button" class="ghost" onclick={cancelNewVideo} disabled={busy}>
                      Cancel
                    </button>
                  </form>
                {:else}
                  <button class="link" disabled={busy} onclick={() => openNewVideoForm(m.id)}>
                    + Add Video
                  </button>
                {/if}
              </li>
            </ul>
          </li>
        {/each}
      </ol>
    {/if}
    {:else}
      <!-- Board view: one column per workflow state, draggable Video cards. -->
      <section class="board" aria-label="Workflow board">
        {#each course.workflowStates as s (s.id)}
          {@const cards = videosInState(s.id)}
          <div
            class="board-column"
            class:drop-target={dropTargetStateId === s.id}
            role="region"
            aria-label={s.name}
            ondragover={(e) => onColumnDragOver(e, s.id)}
            ondragleave={() => onColumnDragLeave(s.id)}
            ondrop={(e) => onColumnDrop(e, s.id)}
          >
            <header class="board-column-header">
              <span class="board-column-title">{s.name}</span>
              <span class="count">{cards.length}</span>
            </header>
            <ul class="board-cards">
              {#each cards as v (v.id)}
                <li
                  class="board-card"
                  class:dragging={draggingVideoId === v.id}
                  draggable="true"
                  ondragstart={(e) => onCardDragStart(e, v.id)}
                  ondragend={onCardDragEnd}
                >
                  <div class="card-title">{v.title}</div>
                  {#if moduleTitleForVideo(v.id)}
                    <div class="card-meta">{moduleTitleForVideo(v.id)}</div>
                  {/if}
                </li>
              {/each}
              {#if cards.length === 0}
                <li class="board-empty">Drag here</li>
              {/if}
            </ul>
          </div>
        {/each}
      </section>
    {/if}
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', sans-serif;
    background: #fafafa;
    color: #111;
  }
  main {
    max-width: 920px;
    margin: 0 auto;
    padding: 2rem 1.5rem;
  }
  header {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-bottom: 1.5rem;
  }
  h1 { font-size: 1.5rem; margin: 0; }
  .folder {
    font-family: 'SF Mono', Menlo, monospace;
    font-size: 0.75rem;
    color: #666;
    background: #f0f0f0;
    padding: 1px 5px;
    border-radius: 3px;
    align-self: flex-start;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .toolbar { margin-bottom: 1rem; }
  .view-toolbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
    gap: 0.75rem;
  }
  .view-toggle {
    display: inline-flex;
    border: 1px solid #ccc;
    border-radius: 6px;
    overflow: hidden;
    background: white;
  }
  .view-toggle button {
    background: white;
    border: none;
    padding: 0.35rem 0.8rem;
    cursor: pointer;
    font-size: 0.85rem;
    color: #444;
  }
  .view-toggle button + button { border-left: 1px solid #ddd; }
  .view-toggle button.active {
    background: #0066ff;
    color: white;
  }

  /* Workflow state badge (tree view) */
  .state-badge {
    display: inline-block;
    margin-left: 0.5rem;
    padding: 1px 7px;
    font-size: 0.7rem;
    color: #444;
    background: #eef2ff;
    border: 1px solid #d8dffb;
    border-radius: 10px;
    white-space: nowrap;
  }

  /* State editor */
  .state-editor {
    background: white;
    border: 1px solid #e0e0e0;
    border-radius: 10px;
    padding: 1rem 1.2rem;
    margin-bottom: 1rem;
  }
  .state-editor header {
    display: flex;
    flex-direction: row;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.75rem;
  }
  .state-editor h2 {
    font-size: 1rem;
    margin: 0;
  }
  ol.states {
    list-style: none;
    padding: 0;
    margin: 0 0 0.75rem 0;
  }
  li.state-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 0.4rem 0;
    border-bottom: 1px solid #f1f1f1;
  }
  li.state-row:last-child { border-bottom: none; }

  /* Board view */
  .board {
    display: flex;
    gap: 0.75rem;
    overflow-x: auto;
    padding-bottom: 0.5rem;
  }
  .board-column {
    flex: 0 0 220px;
    background: #f4f5f7;
    border: 1px solid #e3e5e8;
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    min-height: 200px;
    transition: background-color 100ms ease, border-color 100ms ease;
  }
  .board-column.drop-target {
    background: #e6efff;
    border-color: #99baff;
  }
  .board-column-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.55rem 0.7rem;
    border-bottom: 1px solid #e3e5e8;
  }
  .board-column-title {
    font-weight: 600;
    font-size: 0.85rem;
    color: #333;
  }
  ul.board-cards {
    list-style: none;
    padding: 0.5rem;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    flex: 1;
  }
  li.board-card {
    background: white;
    border: 1px solid #d8dadd;
    border-radius: 6px;
    padding: 0.5rem 0.6rem;
    cursor: grab;
    user-select: none;
    box-shadow: 0 1px 1px rgba(0, 0, 0, 0.03);
  }
  li.board-card.dragging { opacity: 0.4; }
  .card-title { font-size: 0.88rem; color: #111; }
  .card-meta { font-size: 0.7rem; color: #777; margin-top: 2px; }
  .board-empty {
    color: #999;
    font-size: 0.75rem;
    text-align: center;
    padding: 1rem 0.5rem;
    border: 1px dashed #d0d0d0;
    border-radius: 6px;
  }
  .empty {
    background: white;
    padding: 3rem 1rem;
    border-radius: 10px;
    text-align: center;
    color: #666;
  }
  ol.modules {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  li.module {
    background: white;
    border-radius: 10px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
    overflow: hidden;
  }
  .module-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid #eee;
    background: #fbfbfb;
  }
  .module-title {
    font-size: 1.05rem;
    font-weight: 600;
  }
  .count {
    color: #888;
    font-size: 0.8rem;
    margin-left: 0.5rem;
  }
  ul.videos {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  li.video, li.add-video {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    padding: 0.55rem 1rem;
    border-bottom: 1px solid #f1f1f1;
  }
  li.video:last-child, li.add-video {
    border-bottom: none;
  }
  .title-area {
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  button.primary {
    background: #0066ff;
    color: white;
    border: none;
    padding: 0.5rem 1rem;
    border-radius: 6px;
    font-size: 0.9rem;
    cursor: pointer;
  }
  button.primary:disabled { opacity: 0.5; cursor: not-allowed; }
  button.ghost {
    background: white;
    color: #333;
    border: 1px solid #ccc;
    padding: 0.5rem 1rem;
    border-radius: 6px;
    font-size: 0.9rem;
    cursor: pointer;
  }
  button.ghost:disabled { opacity: 0.5; cursor: not-allowed; }
  .new-form {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
  .new-form input {
    flex: 1;
    max-width: 320px;
    font-size: 1rem;
    padding: 0.45rem 0.6rem;
    border: 1px solid #ccc;
    border-radius: 4px;
  }
  button.icon {
    background: none;
    border: 1px solid #ddd;
    width: 26px;
    height: 26px;
    border-radius: 4px;
    cursor: pointer;
    color: #444;
    font-size: 0.85rem;
  }
  button.icon:disabled { opacity: 0.4; cursor: not-allowed; }
  button.link {
    background: none;
    border: none;
    color: #0066ff;
    cursor: pointer;
    padding: 0;
    font-size: 0.85rem;
  }
  button.link.danger { color: #b00020; }
  button.link:disabled { opacity: 0.5; cursor: not-allowed; }
  button.title-btn {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: #111;
    font-size: 0.95rem;
    text-align: left;
  }
  button.title-btn:hover { text-decoration: underline; }
  select {
    font-size: 0.8rem;
    padding: 2px 4px;
    border: 1px solid #ccc;
    border-radius: 4px;
    background: white;
    cursor: pointer;
  }
  input {
    font-size: 0.95rem;
    padding: 2px 6px;
    border: 1px solid #ccc;
    border-radius: 4px;
    width: 100%;
    box-sizing: border-box;
  }
  .error {
    background: #ffe6e6;
    color: #b00020;
    padding: 0.75rem 1rem;
    border-radius: 6px;
    margin-bottom: 1rem;
    font-size: 0.85rem;
  }
</style>
