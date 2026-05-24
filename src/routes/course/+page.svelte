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
    type Course,
    type Module,
    type Video
  } from '$lib/api';

  let folder = $state<string | null>(null);
  let course = $state<Course | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

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
