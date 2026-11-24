<script setup lang="ts">
import { t } from './i18n'
import { ExternalLink, RefreshCw, Search } from '@lucide/vue'
import { invoke } from '@tauri-apps/api/core'
import { computed, ref, watch } from 'vue'

type Skill = { id: string, name: string, kind: string, purpose?: string, categories?: string[], source: string, version: string }

const props = defineProps<{ paths: string, catalogRevision: number }>()
const emit = defineEmits<{ openSource: [url: string], catalogSynced: [] }>()
const skills = ref<Skill[]>([])
const search = ref('')
const loading = ref(false)
const error = ref('')
const catalogFile = ref<[path: string, exists: boolean] | null>(null)
const fileError = ref('')
const syncing = ref(false)
const syncStatus = ref('')

async function syncCatalog() {
  syncing.value = true
  syncStatus.value = ''
  try {
    const updated = await invoke<boolean>('sync_skill_catalog', { force: true })
    emit('catalogSynced')
    syncStatus.value = updated ? t('Catalog synced.') : t('Could not sync any catalog sources.')
  } catch (cause) {
    syncStatus.value = String(cause)
  } finally {
    syncing.value = false
  }
}
const filtered = computed(() => {
  const query = search.value.trim().toLocaleLowerCase()
  return skills.value.filter(skill => [skill.name, skill.id, skill.purpose, skill.source]
    .some(value => value?.toLocaleLowerCase().includes(query)))
})

watch([() => props.paths, () => props.catalogRevision], async ([paths], _, onCleanup) => {
  let active = true
  onCleanup(() => { active = false })
  loading.value = true
  error.value = ''
  try {
    const catalog = await invoke<{ entries: Skill[], errors: string[] }>('load_catalog', {
      extensionPaths: paths.split('\n').map(path => path.trim()).filter(Boolean),
    })
    if (active) {
      skills.value = catalog.entries.filter(entry => entry.kind === 'skill' && !entry.categories?.includes('archived-skill'))
        .sort((a, b) => a.name.localeCompare(b.name))
      error.value = catalog.errors.join('\n')
    }
  } catch (cause) {
    if (active) error.value = String(cause)
  } finally {
    if (active) loading.value = false
  }
}, { immediate: true })

watch(() => props.catalogRevision, async (_, __, onCleanup) => {
  let active = true
  onCleanup(() => { active = false })
  fileError.value = ''
  try {
    const file = await invoke<[string, boolean] | null>('skill_catalog_file')
    if (active) catalogFile.value = file
  } catch (cause) {
    if (active) fileError.value = String(cause)
  }
}, { immediate: true })

async function openCatalogFile() {
  fileError.value = ''
  try {
    await invoke('open_skill_catalog_file')
  } catch (cause) {
    fileError.value = String(cause)
  }
}

function sourceLabel(skill: Skill) {
  const match = skill.source.match(/^https:\/\/github\.com\/([^/]+\/[^/]+)/)
  return match?.[1] ?? t('Catalog')
}
</script>

<template>
  <section class="skill-library" aria-labelledby="skill-library-title">
    <header class="library-head">
      <div>
        <h1 id="skill-library-title">{{ t("Skill library") }}</h1>
        <p v-if="skills.length" class="skill-source">{{ t('{shown} of {total} skills', { shown: filtered.length, total: skills.length }) }}</p>
      </div>
      <label class="library-search">
        <Search :size="16" aria-hidden="true" />
        <input v-model="search" type="search" :aria-label="t('Search skills')" :placeholder="t('Search skills')" />
      </label>
      <button type="button" class="sync-button" :disabled="syncing" @click="syncCatalog">
        <RefreshCw :size="16" :class="{ 'animate-spin': syncing }" aria-hidden="true" />
        {{ syncing ? t('Syncing…') : t('Sync') }}
      </button>
    </header>
    <p v-if="syncStatus" class="skill-source mb-3" role="status">{{ syncStatus }}</p>
    <div v-if="catalogFile" class="catalog-file">
      <button v-if="catalogFile[1]" type="button" class="skill-link catalog-path" @click="openCatalogFile">{{ catalogFile[0] }}</button>
      <template v-else>
        <p class="skill-source">{{ t("Sync to create the local catalog file.") }}</p>
        <p class="skill-source">{{ catalogFile[0] }}</p>
      </template>
    </div>
    <p v-if="fileError" class="text-sm text-red-800" role="alert">{{ fileError }}</p>
    <p v-if="error" class="text-sm text-red-800 whitespace-pre-wrap" role="alert">{{ error }}</p>
    <p v-if="loading && !skills.length" role="status" class="empty-library">{{ t("Loading skills…") }}</p>
    <p v-else-if="!filtered.length" class="empty-library">{{ t("No skills found.") }}</p>
    <ul v-else class="skill-grid">
      <li v-for="skill in filtered" :key="skill.id" class="library-card">
        <p class="skill-source">{{ sourceLabel(skill) }}</p>
        <h2>{{ skill.name }}</h2>
        <p v-if="skill.purpose" class="skill-description">{{ skill.purpose }}</p>
        <button type="button" class="skill-link" :aria-label="t('Open source for {name}', { name: skill.name })" @click="emit('openSource', skill.source)">
          {{ t("View source") }} <ExternalLink :size="14" aria-hidden="true" />
        </button>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.skill-library { padding-block: 24px; max-width: 1440px; }
.catalog-file { display: grid; justify-items: start; gap: 4px; margin-bottom: 20px; }
.catalog-path { max-width: 100%; text-align: left; overflow-wrap: anywhere; }
.sync-button { display: inline-flex; align-items: center; gap: 7px; padding: 8px 12px; border: 1px solid var(--border-strong); border-radius: 8px; background: white; }
.sync-button:not(:disabled):hover { background: var(--surface-hover); }
.sync-button:disabled { opacity: .65; }
.library-head { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 14px; margin-bottom: 22px; }
.library-head h1 { font-size: 25px; font-weight: 700; }
.library-search { display: flex; align-items: center; gap: 8px; width: min(100%, 20rem); margin-left: auto; padding: 8px 11px; border: 1px solid var(--border-strong); border-radius: 8px; background: white; color: var(--text-muted); }
.library-search input { min-width: 0; width: 100%; color: var(--text-strong); }
.library-search:focus-within { outline: 2px solid var(--selection-border); }
.library-search input:focus { outline: none; }
.skill-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(235px, 100%), 1fr)); gap: 12px; }
.library-card { display: flex; flex-direction: column; gap: 6px; padding: 13px; background: white; border: 1px solid var(--border); border-radius: 9px; }
.library-card h2 { font-weight: 650; overflow-wrap: anywhere; }
.skill-source, .skill-description, .empty-library { font-size: 13px; color: var(--text-muted); overflow-wrap: anywhere; }
.skill-link { display: inline-flex; align-items: center; gap: 5px; align-self: start; margin-top: auto; padding-top: 5px; font-size: 13px; text-decoration: underline; }
</style>
