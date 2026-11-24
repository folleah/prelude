<script setup lang="ts">
import { t } from './i18n'
import { moduleDescription } from './module-descriptions'
import { ListChecks, Network, PanelsTopLeft, Puzzle, Server, Terminal, UserRound, Wrench } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'

type Selection = { id: string, version: string, reason: string }
type HookCheck = 'test' | 'lint' | 'typecheck'
type Role = { name: string, responsibility: string, interaction: string, reason: string, modelId?: string, parallelWith?: string[] }
type Plan = { harness: Selection, components: Selection[], roles: Role[], instructions: string[] }
type Entry = {
  id: string
  name: string
  kind: string
  categories?: string[]
  purpose?: string
  source: string
  version: string
  requires: string[]
  conflicts: string[]
  harnessVersions: Record<string, string>
}

const emit = defineEmits<{
  openSource: [url: string]
  canvasElements: [visible: boolean]
  selectModule: [value: { id: string, name: string, version: string, reason: string, purpose?: string, source?: string, requires?: string[], conflicts?: string[], model?: string, interaction?: string, parallelWith?: string[] } | undefined]
}>()
const props = defineProps<{ planText: string, paths: string[], selectedId?: string, localSkills?: string[], hookChecks: HookCheck[], detectedIds?: string[], catalogRevision: number }>()
const entries = ref<Entry[]>([])
const installedIds = ref(new Set<string>())
const plan = computed<Plan | undefined>(() => {
  try {
    const candidate = JSON.parse(props.planText)
    if (typeof candidate?.harness?.id === 'string' && Array.isArray(candidate.components)
      && candidate.components.every((item: Selection) => typeof item?.id === 'string')) return candidate
  } catch {
    // The JSON editor shows validation errors.
  }
})

watch([() => props.paths, () => props.catalogRevision], async ([paths], _, onCleanup) => {
  let active = true
  onCleanup(() => { active = false })
  entries.value = []
  try {
    const result = await invoke<{ entries: Entry[], errors: string[], installedIds?: string[] }>('load_catalog', { extensionPaths: paths })
    if (active) {
      entries.value = result.entries
      installedIds.value = new Set(result.installedIds)
    }
  } catch {
    // Cards can still display their catalog IDs when metadata is unavailable.
  }
}, { immediate: true })

const byId = computed(() => new Map(entries.value.map(entry => [entry.id, entry])))
const zones = [
  { key: 'role', label: 'Orchestration', icon: Network },
  { key: 'harness', label: 'Harness', icon: Terminal },
  { key: 'skill', label: 'Skills and plugins', icon: Puzzle },
  { key: 'tool', label: 'Tools and services', icon: Wrench },
  { key: 'hook', label: 'Hooks', icon: ListChecks },
  { key: 'infrastructure', label: 'Infrastructure', icon: Server },
]
const hookLabels = { test: 'Run tests', lint: 'Run lint', typecheck: 'Check types' } satisfies Record<HookCheck, string>
function category(entry?: Entry) {
  if (entry?.kind === 'tool' && entry.categories?.includes('infrastructure')) return 'infrastructure'
  if (entry?.kind === 'tool' && entry.categories?.includes('session-management')) return 'sessions'
  if (entry?.categories?.includes('orchestration') && entry.kind === 'tool') return 'role'
  const kind = entry?.kind
  return kind === 'model' || kind === 'skill' || kind === 'harness' ? kind : 'tool'
}
function supportsSelectedHarness(entry: Entry) {
  const harness = plan.value?.harness.id
  return !harness || !Object.keys(entry.harnessVersions).length || Boolean(entry.harnessVersions[harness])
}
const parallelNames = computed(() => {
  const roles = plan.value?.roles ?? []
  const names = new Set(roles.map(role => role.name))
  const parallel = new Set<string>()
  for (const role of roles) {
    for (const peer of role.parallelWith ?? []) {
      if (peer !== role.name && names.has(peer)) {
        parallel.add(role.name)
        parallel.add(peer)
      }
    }
  }
  return [...parallel]
})
const parallelKey = computed(() => JSON.stringify(parallelNames.value))
const dismissedHint = ref('')
const showSessionOptions = ref(false)
const sessionManagers = computed(() => entries.value.filter(entry => category(entry) === 'sessions' && entry.categories?.includes('free-tool')
  && supportsSelectedHarness(entry) && (entry.id !== 'cmux' || /Mac/.test(navigator.userAgent) || installedIds.value.has(entry.id)))
  .sort((a, b) => Number(installedIds.value.has(b.id)) - Number(installedIds.value.has(a.id))
    || Number(b.id === 't3-code') - Number(a.id === 't3-code')))
const sessionHintVisible = computed(() => parallelNames.value.length > 1 && sessionManagers.value.length > 0 && dismissedHint.value !== parallelKey.value)
watch(parallelKey, () => { showSessionOptions.value = false })
function cards(zone: string) {
  if (!plan.value) return []
  if (zone === 'harness') return plan.value.harness.id ? [{ item: plan.value.harness, index: -1 }] : []
  return plan.value.components.map((item, index) => ({ item, index }))
    .filter(({ item }) => category(byId.value.get(item.id)) === zone)
}
watch(computed(() => Boolean(props.hookChecks.length || props.localSkills?.length || plan.value?.roles.length || zones.some(zone => cards(zone.key).length))),
  visible => emit('canvasElements', visible), { immediate: true })

function selectModule(item: Selection) {
  const entry = byId.value.get(item.id)
  emit('selectModule', { ...item, name: entry?.name ?? item.id, purpose: entry?.purpose, source: entry?.source, requires: entry?.requires, conflicts: entry?.conflicts })
}

function clearCanvasSelection(event: MouseEvent) {
  if (event.target instanceof Element && !event.target.closest('button, input, select, label, textarea')) {
    emit('selectModule', undefined)
  }
}
function showRole(index: number, role: Role) {
  emit('selectModule', { id: 'role-' + index, name: role.name || t('New role'), version: '', reason: role.responsibility, model: role.modelId ? byId.value.get(role.modelId)?.name ?? role.modelId : undefined, interaction: role.interaction, parallelWith: role.parallelWith })
}

function showHook(check: HookCheck) {
  emit('selectModule', {
    id: 'hook:' + check,
    name: t(hookLabels[check]),
    version: '',
    reason: t('Planned by AI. Exported as a Codex Stop hook when a supported package manager is selected.'),
  })
}
</script>

<template>
  <div v-if="plan" class="canvas" @click="clearCanvasSelection">
  <section v-for="zone in zones" :key="zone.key" class="canvas-zone">
    <h4 class="layer-divider"><component :is="zone.icon" :size="14" aria-hidden="true" />{{ t(zone.label) }}</h4>
    <div class="layer-cards">
      <template v-if="zone.key === 'role'">
        <article v-for="(role, index) in plan.roles" :key="index" class="module-card" :class="{ 'is-selected': selectedId === 'role-' + index }">
          <button type="button" class="card-title" :aria-pressed="selectedId === 'role-' + index" @click="showRole(index, role)"><UserRound :size="16" aria-hidden="true" />{{ role.name || t('New role') }}</button>
        </article>
      </template>
      <article v-for="{ item } in cards(zone.key)" :key="item.id" class="module-card" :class="{ 'is-selected': selectedId === item.id }">
        <button type="button" class="card-title" :aria-pressed="selectedId === item.id" @click="selectModule(item)"><component :is="zone.icon" :size="16" aria-hidden="true" />{{ byId.get(item.id)?.name || item.id }}</button>
        <small v-if="detectedIds?.includes(item.id)" class="installed-label">{{ t('Found in project') }}</small>
      </article>
      <template v-if="zone.key === 'skill'">
        <article v-for="name in localSkills" :key="name" class="module-card">
          <button type="button" class="card-title" :aria-pressed="selectedId === 'local:' + name"
            @click="emit('selectModule', { id: 'local:' + name, name, version: '', reason: 'Existing local skill. Its files stay in the project directory.' })">
            <Puzzle :size="16" aria-hidden="true" />{{ name }}
          </button>
          <small class="installed-label">{{ t('Found in project') }}</small>
        </article>
      </template>
      <template v-if="zone.key === 'hook'">
        <article v-for="check in hookChecks" :key="check" class="module-card">
          <button type="button" class="card-title" :aria-pressed="selectedId === 'hook:' + check" @click="showHook(check)">
            <ListChecks :size="16" aria-hidden="true" />{{ t(hookLabels[check]) }}
          </button>
        </article>
      </template>
    </div>
    <aside v-if="zone.key === 'role' && sessionHintVisible" class="session-hint" :aria-label="t('Session management suggestion')">
      <PanelsTopLeft :size="16" aria-hidden="true" />
      <div>
        <p>{{ t('{roles} can work in parallel. {tool} can help manage their sessions.', { roles: parallelNames.join(', '), tool: sessionManagers[0].name }) }}</p>
        <div class="session-hint-actions">
          <button type="button" class="underline" :aria-expanded="showSessionOptions" @click="showSessionOptions = !showSessionOptions">{{ t("View options") }}</button>
          <button type="button" @click="dismissedHint = parallelKey">{{ t("Dismiss") }}</button>
        </div>
        <ul v-if="showSessionOptions" class="session-options">
          <li v-for="tool in sessionManagers" :key="tool.id">
            <button type="button" class="underline" @click="emit('openSource', tool.source)">{{ tool.name }}</button>
            <span v-if="installedIds.has(tool.id)" class="installed-label"> · {{ t('Detected locally') }}</span>
            <p v-if="tool.purpose">{{ moduleDescription(tool.purpose) ?? tool.purpose }}</p>
          </li>
        </ul>
      </div>
    </aside>
  </section>
  </div>
</template>

<style scoped>
.canvas { min-height: 100%; padding: 16px clamp(12px, 3vw, 36px) 80px; }
.canvas-zone { min-width: 0; min-height: 112px; font-size: 13px; }
.layer-divider { display: flex; align-items: center; gap: 14px; margin: 0; color: var(--text-muted); font-size: 12px; font-weight: 600; letter-spacing: .04em; text-align: center; }
.layer-divider::before, .layer-divider::after { content: ""; flex: 1; height: 1px; background: var(--border); }
.layer-cards { display: flex; flex-wrap: wrap; justify-content: center; align-items: stretch; gap: 10px; padding: 16px 0 20px; }
.module-card { display: grid; flex: 0 1 155px; min-width: 0; max-width: 100%; }
.card-title { display: flex; align-items: center; gap: 7px; width: 100%; padding: 9px; border: 1px solid var(--border); border-radius: 8px; background: white; box-shadow: 0 2px 6px #18181b12; text-align: left; overflow-wrap: anywhere; font-weight: 600; }
.card-title:hover { border-color: var(--border-strong); }
.card-title[aria-pressed="true"] { border-color: var(--selection-border); box-shadow: 0 0 0 1px var(--selection-border); background: var(--surface-selected); }
.card-title svg { flex: none; color: var(--text-muted); }
.installed-label { color: var(--text-medium); font-size: 11px; }
.session-hint { display: flex; align-items: flex-start; gap: 9px; width: fit-content; max-width: 100%; margin: -8px auto 16px; padding: 10px 12px; background: #fff; border: 1px solid var(--border); border-radius: 9px; box-shadow: 0 4px 12px #18181b12; color: var(--text-medium); }
.session-hint > svg { flex: none; margin-top: 2px; }
.session-hint-actions { display: flex; gap: 12px; margin-top: 6px; }
.session-options { display: grid; gap: 8px; margin-top: 8px; }
.session-options > li { border-top: 1px solid var(--border-subtle); padding-top: 6px; }
</style>
