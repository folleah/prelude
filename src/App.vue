<script setup lang="ts">
import { language, setLanguage, t } from './i18n'
import { moduleDescription } from './module-descriptions'
import { ArrowLeft, FileText, Folder, LoaderCircle, LogOut, MessageSquare, Settings, Trash2, X } from '@lucide/vue'
import { invoke } from '@tauri-apps/api/core'
import { computed, nextTick, onMounted, ref, toRaw, watch } from 'vue'
import PlanBuilder from './PlanBuilder.vue'
import type { DeclaredDependency, ProjectDependency } from './project-dependency'
import type { HookCheck, PlannedHook } from './planned-hook'

const idea = ref('')
const projectDescription = ref('')
type Discovery = { description: string, questions: string[], answers: string[] }
type ChatMessage = { text: string, role: 'user' | 'assistant' | 'error' }
const discovery = ref<Discovery>()
const projectTitle = ref('')
const error = ref('')
type Selection = { id: string, version: string, reason: string }
const hookChecks = ref<PlannedHook[]>([])
type Role = { name: string, responsibility: string, interaction: string, reason: string, modelId?: string, parallelWith?: string[] }
type Plan = { harness: Selection, components: Selection[], roles: Role[], instructions: string[], dependencies?: ProjectDependency[] }
type GenerationInput = { prompt: string, answers: [string, string][], useDefaults: boolean, basePlan?: Plan }
const generationInput = ref<GenerationInput>()
function emptyPlan(): Plan {
  return { harness: { id: '', version: '', reason: '' }, components: [], roles: [], instructions: [] }
}
type Recommendation = {
  plan: Plan
  notes: string[]
  offline: boolean
}
const project = ref<Recommendation>()
const localSkills = ref<string[]>([])
const catalogRevision = ref(0)
const inspecting = ref(false)
const screen = ref<'connect' | 'projects' | 'builder' | 'settings'>('connect')
const restoringConnection = ref(true)
const openingProject = ref(false)
const yamlInput = ref<HTMLInputElement>()
const planText = ref('')
const builderPaths = ref<string[]>([])
const newDescription = ref('')
const promptDialog = ref<HTMLDialogElement>()
const analyzedProject = ref(false)
const projectSources = ref<ProjectSource[]>([])
const projectTechnologies = ref<string[]>([])
const declaredDependencies = ref<DeclaredDependency[]>([])
const detectedIds = ref<string[]>([])

function clearAnalysisContext() {
  analyzedProject.value = false
  projectSources.value = []
  projectTechnologies.value = []
  declaredDependencies.value = []
  detectedIds.value = []
}

async function newProject() {
  if (deliveryBusy.value || inspecting.value || openingProject.value) return
  initializeProject()
  newDescription.value = ''
  await nextTick()
  promptDialog.value?.showModal()
}

function startProject() {
  const description = newDescription.value.trim()
  if (!description) return
  promptDialog.value?.close()
  idea.value = description
  void inspectProject()
}

function initializeProject() {
  activeHistoryId.value = crypto.randomUUID()
  generatedInstructions.value = undefined
  clearAnalysisContext()
  loadProject({
    schemaVersion: 1, description: '', assumptions: [],
    plan: emptyPlan(),
  })
  workspaceStatus.value = ''
  error.value = ''
}

type ProjectYaml = {
  schemaVersion: number, title?: string, description: string, plan: Plan, assumptions: string[],
  localSkills?: string[], messages?: ChatMessage[], discovery?: Discovery, deliveryMode?: 'create' | 'apply',
  hookChecks?: (HookCheck | PlannedHook)[],
  generationInput?: GenerationInput,
}
type ProjectSource = { path: string, scope: string, content: string }
type OpenedProject = {
  workspace: ProjectYaml | null, plan: Plan, notes: string[], localSkills: string[],
  sources: ProjectSource[], technologies: string[], detectedIds: string[], dependencies: DeclaredDependency[],
}
const workspaceStatus = ref('')
const historyPrefix = 'prelude.draft-history.v1.'
type HistoryDraft = {
  id: string, name: string, updated: number, project: ProjectYaml, draft: string, paths: string[],
  analysis?: { sources: ProjectSource[], technologies: string[], detectedIds: string[], dependencies?: DeclaredDependency[] },
  instructions?: InstructionPreview,
  exports?: Partial<Record<ExportTarget, AgentExport>>,
}
const history = ref<HistoryDraft[]>([])
const activeHistoryId = ref<string>()

function readHistory() {
  const drafts: HistoryDraft[] = []
  for (let index = 0; index < localStorage.length; index++) {
    const key = localStorage.key(index)
    if (!key?.startsWith(historyPrefix)) continue
    try {
      const draft = JSON.parse(localStorage.getItem(key) ?? 'null') as HistoryDraft | null
      if (draft?.project?.schemaVersion === 1 && draft.project.plan && key === historyPrefix + draft.id) drafts.push(draft)
    } catch {
      // A damaged entry does not prevent other drafts from opening.
    }
  }
  history.value = drafts.sort((a, b) => b.updated - a.updated)
}

function openHistory(saved: HistoryDraft) {
  const item = structuredClone(toRaw(saved))
  activeHistoryId.value = item.id
  clearAnalysisContext()
  loadProject(item.project)
  idea.value = item.draft
  planCatalogPaths = [...item.paths]
  builderPaths.value = [...item.paths]
  if (item.analysis) {
    analyzedProject.value = true
    projectSources.value = item.analysis.sources
    projectTechnologies.value = [...item.analysis.technologies]
    declaredDependencies.value = item.analysis.dependencies ?? []
    detectedIds.value = [...item.analysis.detectedIds]
  }
  generatedInstructions.value = item.instructions
  const exports = item.exports ?? {}
  if (exports.codex?.files['.codex/hooks/run-tests.cjs']) delete exports.codex
  exportDrafts.value = exports
  workspaceStatus.value = ''
}

function removeHistory(id: string) {
  try {
    localStorage.removeItem(historyPrefix + id)
    history.value = history.value.filter(item => item.id !== id)
    if (activeHistoryId.value === id) activeHistoryId.value = undefined
  } catch (cause) {
    workspaceStatus.value = String(cause)
  }
}

function snapshotProject(): ProjectYaml {
  return {
    schemaVersion: 1, title: projectTitle.value.trim() || undefined, description: projectDescription.value || discovery.value?.description || idea.value, plan: JSON.parse(planText.value), assumptions: project.value?.notes ?? [],
    localSkills: [...localSkills.value],
    hookChecks: [...hookChecks.value],
    messages: messages.value.map(message => ({ ...message })),
    discovery: discovery.value && { ...discovery.value, questions: [...discovery.value.questions], answers: [...discovery.value.answers] },
    generationInput: generationInput.value,
  }
}

function loadProject(snapshot: ProjectYaml) {
  const title = snapshot.title?.trim() ?? ''
  const firstPrompt = snapshot.messages?.find(message => message.role === 'user')?.text
  const clippedPrompt = [snapshot.description, firstPrompt].some(text => {
    const normalized = text?.trim().replace(/\s+/g, ' ') ?? ''
    return (normalized.length > 56 && title === normalized.slice(0, 56)) || (title.endsWith('?') && title === normalized)
  })
  projectTitle.value = clippedPrompt ? '' : title
  localSkills.value = snapshot.localSkills ?? []
  hookChecks.value = snapshot.hookChecks?.map(hook =>
    typeof hook === 'string' ? { check: hook, title: '', description: '' } : hook) ?? []
  projectDescription.value = snapshot.discovery ? '' : snapshot.description
  discovery.value = snapshot.discovery
  generationInput.value = snapshot.generationInput
  planCatalogPaths = []
  builderPaths.value = []
  idea.value = ''
  project.value = { plan: snapshot.plan, notes: snapshot.assumptions, offline: true }
  messages.value = snapshot.messages ?? (snapshot.description ? [{ text: snapshot.description, role: 'user' }] : [])
  selectedModule.value = undefined
  planText.value = JSON.stringify(snapshot.plan, null, 2)
  screen.value = 'builder'
}

async function openExistingProject() {
  if (openingProject.value || inspecting.value) return
  openingProject.value = true
  workspaceStatus.value = ''
  try {
    const directory = await invoke<string | null>('pick_project_directory', { current: null })
    if (!directory) return
    const opened = await invoke<OpenedProject>('open_project_directory', { repository: directory, extensionPaths: [] })
    initializeProject()
    loadProject({
      schemaVersion: 1, title: opened.workspace?.title, description: opened.workspace?.description ?? '',
      plan: opened.plan, assumptions: opened.notes, localSkills: opened.localSkills,
      messages: [{ role: 'assistant', text: [t('Project directory analyzed.'), ...opened.notes].join('\n') }],
    })
    analyzedProject.value = true
    projectSources.value = opened.sources
    projectTechnologies.value = opened.technologies
    declaredDependencies.value = opened.dependencies
    detectedIds.value = opened.detectedIds
  } catch (cause) {
    workspaceStatus.value = String(cause)
  } finally {
    openingProject.value = false
  }
}

function showProjects() {
  if (!inspecting.value && !deliveryBusy.value && !openingProject.value) screen.value = 'projects'
}

async function saveYaml() {
  try {
    const yaml = await invoke<string>('encode_project_yaml', { project: snapshotProject() })
    downloadFile('prelude.yaml', yaml, 'application/yaml')
  } catch (cause) {
    workspaceStatus.value = String(cause)
  }
}

async function openYaml(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file || inspecting.value) return
  try {
    if (file.size > 1024 * 1024) throw new Error(t('The project YAML exceeds 1 MiB.'))
    const snapshot = await invoke<ProjectYaml>('decode_project_yaml', { yaml: await file.text() })
    const current = JSON.parse(planText.value) as Plan
    const hasConfiguration = current.harness.id || current.components.length || current.roles.length || current.instructions.length || current.dependencies?.length || localSkills.value.length
    if (hasConfiguration && !window.confirm(t('Replace the current configuration?'))) return
    loadProject(snapshot)
    clearAnalysisContext()
    generatedInstructions.value = undefined
    workspaceStatus.value = ''
  } catch (cause) {
    workspaceStatus.value = String(cause)
  }
}
type InstructionPreview = { path: string, content: string }
type ExportTarget = 'codex' | 'claude-code' | 'opencode'
type AgentExport = { files: Record<string, string>, omitted: string[], skillErrors?: string[], roleLayoutVersion?: number }
const exportTargets = [
  { id: 'codex', name: 'OpenAI Codex' },
  { id: 'claude-code', name: 'Claude Code' },
  { id: 'opencode', name: 'OpenCode' },
] as const
const deliveryError = ref('')
const deliveryBusy = ref(false)
const instructionDialog = ref<HTMLDialogElement>()
const generatedInstructions = ref<InstructionPreview>()
const exportDrafts = ref<Partial<Record<ExportTarget, AgentExport>>>({})
const exportTarget = ref<ExportTarget>('codex')
const exportError = ref('')
const exportStatus = ref('')
const selectedExportPath = ref('')
const modulePreferencesText = ref('')
const modulePreferencesBusy = ref(false)
const modulePreferencesStatus = ref('')
const modulePreferencesError = ref('')

async function editModulePreferences(action: 'load' | 'defaults' | 'save') {
  modulePreferencesBusy.value = true
  modulePreferencesStatus.value = ''
  modulePreferencesError.value = ''
  try {
    modulePreferencesText.value = action === 'save'
      ? await invoke<string>('save_module_preferences', { text: modulePreferencesText.value })
      : await invoke<string>('read_module_preferences', { defaults: action === 'defaults' })
    if (action === 'save') modulePreferencesStatus.value = t('Module preferences saved.')
  } catch (cause) {
    modulePreferencesError.value = String(cause)
  } finally {
    modulePreferencesBusy.value = false
  }
}

watch(screen, value => {
  if (value === 'settings') void editModulePreferences('load')
})
const activeExport = computed(() => exportDrafts.value[exportTarget.value])
const exportTree = computed(() => {
  const rows: { path: string, name: string, depth: number, folder: boolean }[] = []
  const directories = new Set<string>()
  const files = Object.keys(activeExport.value?.files ?? {}).sort((a, b) =>
    Number(a.includes('/')) - Number(b.includes('/')) || a.localeCompare(b))
  for (const path of files) {
    const segments = path.split('/')
    segments.forEach((name, depth) => {
      const folder = depth < segments.length - 1
      const prefix = segments.slice(0, depth + 1).join('/')
      if (folder && directories.has(prefix)) return
      if (folder) directories.add(prefix)
      rows.push({ path: prefix, name, depth, folder })
    })
  }
  return rows
})

watch([planText, projectDescription, localSkills, builderPaths, hookChecks], () => {
  generatedInstructions.value = undefined
  exportDrafts.value = {}
}, { flush: 'sync' })
watch(() => {
  const id = activeHistoryId.value
  if (screen.value !== 'builder' || !id || !project.value || !planText.value) return
  const snapshot = snapshotProject()
  if (!snapshot.description.trim() && !snapshot.messages?.length && !analyzedProject.value && !snapshot.plan.harness.id
    && !snapshot.plan.components.length && !snapshot.plan.roles.length && !snapshot.plan.instructions.length && !snapshot.plan.dependencies?.length) return
  const name = snapshot.title || t('Untitled project')
  return JSON.stringify({
    id, name, project: snapshot, draft: idea.value, paths: builderPaths.value,
    analysis: analyzedProject.value ? {
      sources: projectSources.value, technologies: projectTechnologies.value, detectedIds: detectedIds.value, dependencies: declaredDependencies.value,
    } : undefined,
    instructions: generatedInstructions.value,
    exports: exportDrafts.value,
  })
}, serialized => {
  if (!serialized) return
  const item = { ...JSON.parse(serialized), updated: Date.now() } as HistoryDraft
  try {
    localStorage.setItem(historyPrefix + item.id, JSON.stringify(item))
    history.value = [item, ...history.value.filter(previous => previous.id !== item.id)]
  } catch (cause) {
    workspaceStatus.value = t('Could not save the current project on this device. ') + String(cause)
  }
}, { flush: 'pre' })

function dismissOnBackdrop(event: MouseEvent) {
  const dialog = event.currentTarget as HTMLDialogElement
  if (dialog === instructionDialog.value && deliveryBusy.value) return
  if (event.target !== dialog) return
  const bounds = dialog.getBoundingClientRect()
  if (event.clientX < bounds.left || event.clientX >= bounds.right
    || event.clientY < bounds.top || event.clientY >= bounds.bottom) dialog.close()
}

watch([planText, builderPaths], () => { deliveryError.value = '' }, { flush: 'sync' })

async function prepareInstructions() {
  if (deliveryBusy.value || inspecting.value || checkingPlan.value) return
  deliveryError.value = ''
  const currentTarget = snapshotProject().plan.harness.id
  if (exportTargets.some(target => target.id === currentTarget)) exportTarget.value = currentTarget as ExportTarget
  else exportTarget.value = 'codex'
  instructionDialog.value?.showModal()
  await loadExport()
}

async function loadExport() {
  exportError.value = ''
  exportStatus.value = ''
  selectedExportPath.value = ''
  const target = exportTarget.value
  const instructionsPath = target === 'claude-code' ? 'CLAUDE.md' : 'AGENTS.md'
  deliveryBusy.value = true
  try {
    const draft = exportDrafts.value[target]
    const workspace = snapshotProject()
    const updateRoleLayout = Boolean(draft && draft.roleLayoutVersion !== 2 && workspace.plan.roles.length > 1)
    if (!draft?.skillErrors || draft.skillErrors.length || updateRoleLayout) {
      const result = await invoke<AgentExport>('prepare_agent_export', {
        workspace, extensionPaths: builderPaths.value, target, projectSources: projectSources.value,
      })
      if (draft) {
        for (const path of Object.keys(result.files)) {
          if (!(path in draft.files)) continue
          if (updateRoleLayout && path === instructionsPath) {
            const section = /^## Roles[ \t]*\r?\n[\s\S]*?(?=^## |$(?![\s\S]))/m
            const roles = result.files[path].match(section)?.[0]
            if (roles) {
              const previous = draft.files[path]
              result.files[path] = section.test(previous) ? previous.replace(section, () => roles) : previous.trimEnd() + '\n\n' + roles
            }
          } else {
            result.files[path] = draft.files[path]
          }
        }
      }
      exportDrafts.value[target] = result
    }
    selectedExportPath.value = instructionsPath
  } catch (cause) {
    exportError.value = String(cause)
  } finally {
    deliveryBusy.value = false
  }
}

async function saveExport() {
  if (!activeExport.value || deliveryBusy.value) return
  deliveryBusy.value = true
  exportError.value = ''
  exportStatus.value = ''
  try {
    const saved = await invoke<boolean>('save_agent_export', { files: activeExport.value.files, dialogTitle: t('Export ZIP') })
    if (saved) exportStatus.value = t('Archive saved.')
  } catch (cause) {
    exportError.value = String(cause)
  } finally {
    deliveryBusy.value = false
  }
}

function downloadFile(name: string, content: string, type: string) {
  const url = URL.createObjectURL(new Blob([content], { type }))
  const link = document.createElement('a')
  link.href = url
  link.download = name
  link.click()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

const hasCanvasElements = ref(false)
const planIssues = ref<string[]>([])
const checkingPlan = ref(false)
let planCatalogPaths: string[] = []
const openRouterEndpoint = 'https://openrouter.ai/api/v1/chat/completions'
const apiEndpoint = ref('')
const endpointPresets = [
  { name: 'OpenRouter', url: openRouterEndpoint },
  { name: 'OpenAI', url: 'https://api.openai.com/v1/chat/completions' },
]
const isOpenRouter = computed(() => apiEndpoint.value.trim().replace(/\/$/, '') === openRouterEndpoint)
const apiModel = ref('')
const apiModelInput = ref('')
const apiPrice = ref<{ input: number, output: number }>()
const apiPricePending = ref(false)
watch([isOpenRouter, apiModel], ([openRouter, model], _, onCleanup) => {
  apiPrice.value = undefined
  apiPricePending.value = openRouter && !!model.trim()
  if (!apiPricePending.value) return
  let active = true
  const timer = setTimeout(async () => {
    try {
      const rates = await invoke<{ input: number, output: number }>('openrouter_prices', { model })
      if (active) apiPrice.value = rates
    } catch {
      // The model remains selectable when the public pricing catalog is unavailable.
    } finally {
      if (active) apiPricePending.value = false
    }
  }, 450)
  onCleanup(() => { active = false; clearTimeout(timer) })
})
const apiKey = ref('')
const apiConnected = ref(false)
const apiStatus = ref('')
const configuring = ref(false)
const acpProviders = { github: 'GitHub Copilot', gemini: 'Gemini', kiro: 'Kiro' } as const
type AcpProvider = keyof typeof acpProviders
function isAcpProvider(value: string | null): value is AcpProvider {
  return value === 'github' || value === 'gemini' || value === 'kiro'
}
const activeConnection = ref<'api' | 'chatgpt' | AcpProvider>('api')
const acpProvider = computed(() => isAcpProvider(activeConnection.value) ? activeConnection.value : null)
const acpStatus = ref('')
watch(activeConnection, () => { acpStatus.value = '' })
const chatgptModel = ref(localStorage.getItem('makemeharness.chatgpt-model') ?? '')
const chatgptConnected = ref(false)
const chatgptStatus = ref('')
const chatgptBusy = ref(false)
const messages = ref<ChatMessage[]>([])
const chatLog = ref<HTMLElement>()
const chatInput = ref<HTMLTextAreaElement>()
watch([() => messages.value.length, inspecting], () => {
  if (chatLog.value) chatLog.value.scrollTop = chatLog.value.scrollHeight
}, { flush: 'post' })
type ModuleDetails = { id: string, name: string, version: string, reason: string, purpose?: string, source?: string, requires?: string[], conflicts?: string[], model?: string, interaction?: string, parallelWith?: string[] }
const selectedModule = ref<ModuleDetails>()
const translatedReason = ref('')
const descriptionCache = new Map<string, string>()
const moduleReason = computed(() => {
  const entry = selectedModule.value
  if (!entry) return ''
  const reason = entry.reason.trim() || entry.purpose || ''
  return translatedReason.value || moduleDescription(reason)
    || (descriptionNeedsTranslation(reason) && entry.purpose ? moduleDescription(entry.purpose) : undefined)
    || reason
})

function descriptionNeedsTranslation(text: string) {
  if (!text || moduleDescription(text)) return false
  return language.value === 'ru' ? !/\p{Script=Cyrillic}/u.test(text) : /\p{Script=Cyrillic}/u.test(text)
}

watch([selectedModule, language], async ([entry, locale], _, onCleanup) => {
  translatedReason.value = ''
  const text = entry?.reason.trim() || entry?.purpose || ''
  if (!descriptionNeedsTranslation(text) || !chatModel.value) return
  const key = JSON.stringify([locale, text])
  const cached = descriptionCache.get(key)
  if (cached) {
    translatedReason.value = cached
    return
  }
  let active = true
  onCleanup(() => { active = false })
  try {
    const result = await invoke<string>('translate_module_description', {
      text, language: locale, chatgptModel: activeConnection.value === 'chatgpt' ? chatModel.value : null, acpProvider: acpProvider.value,
    })
    descriptionCache.set(key, result)
    if (active) translatedReason.value = result
  } catch {
    // An unavailable provider leaves the local catalog description or original text visible.
  }
})
const moduleSource = computed(() => {
  const source = selectedModule.value?.source
  if (!source || !URL.canParse(source)) return
  const url = new URL(source)
  if (url.protocol === "https:" && !url.username && !url.password) return source
})

async function openModuleSource(url: string) {
  try {
    await invoke("open_module_source", { url })
  } catch (cause) {
    error.value = String(cause)
  }
}
const modelOptions = ref<{ id: string, name: string }[]>([])
const loadingModels = ref(false)
const modelError = ref('')
let modelRequest = 0
const chatModel = computed({
  get: () => acpProvider.value ? 'default' : activeConnection.value === 'chatgpt' ? chatgptModel.value : apiModel.value,
  set: value => {
    if (acpProvider.value) return
    if (activeConnection.value === 'chatgpt') chatgptModel.value = value
    else {
      apiModel.value = value
      if (apiConnected.value && value) {
        void invoke('remember_api', { model: value }).catch(cause => { workspaceStatus.value = String(cause) })
      }
    }
  },
})

async function loadModels(restoredModel = ''): Promise<boolean> {
  const revision = ++modelRequest
  loadingModels.value = true
  modelOptions.value = []
  modelError.value = ''
  try {
    const options = await invoke<{ id: string, name: string }[]>('list_provider_models', { useChatgpt: activeConnection.value === 'chatgpt' })
    if (revision !== modelRequest) return false
    modelOptions.value = restoredModel && !options.some(option => option.id === restoredModel)
      ? [{ id: restoredModel, name: restoredModel }, ...options]
      : options
    if (restoredModel) return true
    if (!options.some(option => option.id === chatModel.value)) chatModel.value = options[0]?.id ?? ''
    if (!options.length) modelError.value = t('This provider has no available models.')
    return options.length > 0
  } catch (cause) {
    if (revision !== modelRequest) return false
    if (restoredModel) {
      modelOptions.value = [{ id: restoredModel, name: restoredModel }]
      return true
    }
    modelError.value = String(cause)
    return false
  } finally {
    if (revision === modelRequest) loadingModels.value = false
  }
}

watch(chatgptModel, model => { localStorage.setItem('makemeharness.chatgpt-model', model) })

const connectionPreference = 'makemeharness.connection-method'

async function updateAcpConnection(provider: AcpProvider, action: 'login' | 'check' | 'restore') {
  configuring.value = true
  acpStatus.value = ''
  try {
    if (action === 'login') {
      await invoke('open_acp_login', { provider })
      acpStatus.value = t('Finish signing in in the terminal, then check the connection.')
      return
    }
    await invoke('check_acp_connection', { provider, probe: action === 'check' })
    modelOptions.value = [{ id: 'default', name: t('Provider default') }]
    modelError.value = ''
    localStorage.setItem(connectionPreference, provider)
    if (screen.value === 'connect') screen.value = 'projects'
    acpStatus.value = t('Connected.')
  } catch (cause) {
    acpStatus.value = String(cause)
  } finally {
    configuring.value = false
  }
}
const signingOut = ref(false)
const signOutError = ref('')
const canSignOut = computed(() => !signingOut.value && !configuring.value && !chatgptBusy.value
  && !inspecting.value && !deliveryBusy.value && !openingProject.value)

async function signOut() {
  if (!canSignOut.value) return
  signingOut.value = true
  signOutError.value = ''
  try {
    const [chatgptResult, apiResult] = await Promise.allSettled(acpProvider.value ? [] : [
      invoke('disconnect_chatgpt'),
      invoke('configure_api', { endpoint: '', model: '', key: '' }),
    ])
    if (chatgptResult?.status === 'fulfilled') chatgptConnected.value = false
    if (apiResult?.status === 'fulfilled') {
      apiConnected.value = false
      apiKey.value = ''
    }
    const errors = [chatgptResult, apiResult].filter(result => result?.status === 'rejected')
    if (errors.length) {
      signOutError.value = errors.map(result => String(result.reason)).join('\n')
      return
    }
    if (acpProvider.value) localStorage.setItem(connectionPreference, 'signed-out')
    else localStorage.removeItem(connectionPreference)
    apiStatus.value = ''
    chatgptStatus.value = ''
    acpStatus.value = ''
    modelOptions.value = []
    modelError.value = ''
    screen.value = 'connect'
  } finally {
    signingOut.value = false
  }
}

async function restoreApiConnection(): Promise<boolean> {
  try {
    const saved = await invoke<{ endpoint: string, model: string } | null>('restore_api')
    if (!saved) return false
    apiEndpoint.value = saved.endpoint
    apiModel.value = saved.model
    activeConnection.value = 'api'
    apiConnected.value = true
    screen.value = 'projects'
    void loadModels(saved.model)
    apiStatus.value = t('API provider connection restored.')
    return true
  } catch (cause) {
    apiStatus.value = String(cause)
    return false
  }
}

onMounted(async () => {
  try {
    readHistory()
  } catch (cause) {
    workspaceStatus.value = String(cause)
  }
  void invoke<boolean>('sync_skill_catalog', { force: false }).then(updated => { if (updated) catalogRevision.value++ }).catch(() => undefined)
  const preference = localStorage.getItem(connectionPreference)
  const preferApi = preference === 'api'
  configuring.value = true
  chatgptBusy.value = true
  try {
    if (preference === 'signed-out') return
    if (isAcpProvider(preference)) {
      activeConnection.value = preference
      await updateAcpConnection(preference, 'restore')
      return
    }
    if (preferApi && await restoreApiConnection()) return
    try {
      if (await invoke<boolean>('restore_chatgpt')) {
        chatgptConnected.value = true
        activeConnection.value = 'chatgpt'
        screen.value = 'projects'
        void loadModels()
        chatgptStatus.value = t('ChatGPT session restored.')
        return
      }
    } catch (cause) {
      chatgptStatus.value = String(cause)
      if (preference === 'chatgpt') {
        activeConnection.value = 'chatgpt'
        return
      }
    }
    if (!preferApi) await restoreApiConnection()
  } finally {
    configuring.value = false
    chatgptBusy.value = false
    restoringConnection.value = false
  }
})

async function updateChatGpt(action: 'connect' | 'disconnect' | 'test') {
  chatgptBusy.value = true
  chatgptStatus.value = ''
  try {
    if (action === 'connect') {
      await invoke('connect_chatgpt')
      chatgptConnected.value = true
      activeConnection.value = 'chatgpt'
      screen.value = 'projects'
      void loadModels()
      localStorage.setItem(connectionPreference, 'chatgpt')
      chatgptStatus.value = t('Account connected.')
    } else if (action === 'disconnect') {
      await invoke('disconnect_chatgpt')
      chatgptConnected.value = false
      screen.value = 'connect'
      if (localStorage.getItem(connectionPreference) === 'chatgpt') localStorage.removeItem(connectionPreference)
      chatgptStatus.value = t('Account disconnected.')
    } else {
      await invoke('test_chatgpt', { model: chatgptModel.value })
      chatgptStatus.value = t('Connection verified.')
    }
  } catch (cause) {
    chatgptStatus.value = String(cause)
  } finally {
    chatgptBusy.value = false
  }
}

async function checkApiModel() {
  configuring.value = true
  apiStatus.value = ''
  try {
    await invoke('set_api_model', { model: apiModel.value })
    await invoke('test_api')
    apiStatus.value = t('Connection verified.')
  } catch (cause) {
    apiStatus.value = String(cause)
  } finally {
    configuring.value = false
  }
}

async function configureApi(disconnect = false) {
  configuring.value = true
  apiStatus.value = ''
  try {
    await invoke('configure_api', {
      endpoint: apiEndpoint.value.trim(),
      model: apiModelInput.value.trim(), key: disconnect ? '' : apiKey.value,
    })
    apiConnected.value = false
    if (!disconnect) {
      activeConnection.value = 'api'
      const manualModel = apiModelInput.value.trim()
      const loaded = await loadModels()
      if (manualModel) {
        if (!modelOptions.value.some(option => option.id === manualModel)) {
          await invoke('test_api')
          modelOptions.value = [{ id: manualModel, name: manualModel }, ...modelOptions.value]
          modelError.value = ''
        }
        apiModel.value = manualModel
      } else if (!loaded) {
        apiStatus.value = modelError.value
        return
      }
      await invoke('remember_api', { model: apiModel.value })
      apiKey.value = ''
      apiConnected.value = true
      localStorage.setItem(connectionPreference, 'api')
      screen.value = 'projects'
      apiStatus.value = t('API provider connected.')
    } else {
      apiKey.value = ''
      if (localStorage.getItem(connectionPreference) === 'api') localStorage.removeItem(connectionPreference)
      screen.value = 'connect'
    }
  } catch (cause) {
    apiStatus.value = String(cause)
  } finally {
    configuring.value = false
  }
}
watch(planText, (text, _, onCleanup) => {
  planIssues.value = []
  if (!project.value) {
    checkingPlan.value = false
    return
  }
  checkingPlan.value = true
  let active = true
  const timer = setTimeout(async () => {
    try {
      const plan = JSON.parse(text)
      const issues = await invoke<string[]>('validate_plan', { plan, extensionPaths: planCatalogPaths })
      if (active) planIssues.value = issues
    } catch (cause) {
      if (active) planIssues.value = [String(cause)]
    } finally {
      if (active) checkingPlan.value = false
    }
  }, 250)
  onCleanup(() => {
    active = false
    clearTimeout(timer)
  })
})

const canRequestPlan = computed(() => !inspecting.value && !loadingModels.value && !deliveryBusy.value
  && modelOptions.value.some(model => model.id === chatModel.value))
const canSendPrompt = computed(() => canRequestPlan.value && Boolean(idea.value.trim()))
const canRefreshStructure = computed(() => canRequestPlan.value && !discovery.value && Boolean(projectDescription.value.trim()))

function asksForDiscoveryDefaults(text: string) {
  const normalized = text.toLowerCase().replace(/[.!?…]+$/u, '').trim()
  return /^(?:(?:давай )?(?:выбери (?:сам|сама|по умолчанию)|реши (?:сам|сама)|на тво[её] усмотрение|без (?:уточнений|вопросов))|(?:please )?(?:choose (?:the )?defaults|use (?:the )?defaults|you decide|skip (?:the )?questions))$/u.test(normalized)
}

function handlePromptEnter(event: KeyboardEvent) {
  if (event.isComposing || event.keyCode === 229 || event.ctrlKey || event.metaKey) return
  event.preventDefault()
  const textarea = event.currentTarget as HTMLTextAreaElement
  if (event.shiftKey || event.altKey) {
    textarea.setRangeText('\n', textarea.selectionStart, textarea.selectionEnd, 'end')
    textarea.dispatchEvent(new Event('input', { bubbles: true }))
  } else if (!event.repeat) {
    textarea.form?.requestSubmit()
  }
}

async function inspectProject(mode: 'chat' | 'defaults' | 'refresh' = 'chat') {
  const refresh = mode === 'refresh'
  if (!canRequestPlan.value || (mode === 'chat' && !canSendPrompt.value) || (refresh && !canRefreshStructure.value)) return
  const submitted = idea.value
  const interview = discovery.value
  const seed = refresh ? generationInput.value : undefined
  const previousPrompt = refresh ? messages.value.find(message => message.role === 'user')?.text : undefined
  const draft = refresh
    ? seed?.prompt || previousPrompt || projectDescription.value
    : mode === 'defaults' ? t('Choose defaults') : submitted.trim()
  const skipDiscovery = refresh ? seed?.useDefaults ?? true : mode === 'defaults' || asksForDiscoveryDefaults(draft)
  if (!refresh) {
    if (draft) messages.value.push({ role: 'user', text: draft })
    idea.value = ''
  }
  error.value = ''
  inspecting.value = true
  try {
    if (activeConnection.value === 'api') await invoke('set_api_model', { model: apiModel.value })
    const paths = builderPaths.value
    const chatgptModelId = activeConnection.value === 'chatgpt' ? chatgptModel.value : null
    const profile = {
      idea: draft || null,
      repository: null,
      budget: { amount: 0, currency: 'USD', periodDays: 30 },
      priority: 50,
    }
    const firstAnalyzedRequest = analyzedProject.value && !generationInput.value
    const description = seed?.prompt || interview?.description || (firstAnalyzedRequest
      ? [projectDescription.value, draft].filter(Boolean).join('\n\n')
      : projectDescription.value || draft)
    const answers = interview ? [...interview.answers, ...(skipDiscovery ? [] : [draft])] : []
    const answeredQuestions = interview?.questions.slice(0, answers.length) ?? []
    const discoveryAnswers = seed?.answers ?? answeredQuestions.map((question, index): [string, string] => [question, answers[index]])
    if (!refresh && !skipDiscovery && (interview || firstAnalyzedRequest || !projectDescription.value)) {
      const questions = await invoke<string[]>('discover_project', {
        profile: { ...profile, idea: description }, chatgptModel: chatgptModelId, acpProvider: acpProvider.value, discoveryAnswers,
        projectDependencies: analyzedProject.value ? declaredDependencies.value : null,
        projectTechnologies: analyzedProject.value ? projectTechnologies.value : null,
      })
      if (questions.length) {
        discovery.value = { description, questions: [...answeredQuestions, questions[0]], answers }
        messages.value.push({ role: 'assistant', text: questions[0] })
        return
      }
    }
    const basePlan = planText.value
    const currentPlan = refresh ? seed?.basePlan ?? emptyPlan() : JSON.parse(basePlan) as Plan
    const captureInput = !refresh && !generationInput.value
      && (analyzedProject.value || !currentPlan.harness.id)
    const result = await invoke<{ candidate: { plan: Plan, assumptions: string[] }, projectBrief?: string | null, sessionTitle?: string | null, hookChecks?: PlannedHook[] | null }>('request_plan', {
      current: { plan: currentPlan, assumptions: refresh ? [] : project.value?.notes ?? [] },
      request: draft,
      currentSessionTitle: projectTitle.value || null,
      currentHookChecks: refresh ? [] : hookChecks.value,
      discoveryAnswers,
      useDiscoveryDefaults: skipDiscovery,
      projectSources: analyzedProject.value ? projectSources.value : null,
      projectTechnologies: analyzedProject.value ? projectTechnologies.value : null,
      projectDependencies: analyzedProject.value ? declaredDependencies.value : null,
      interfaceLanguage: language.value,
      extensionPaths: paths,
      confirmed: true,
      chatgptModel: chatgptModelId,
      acpProvider: acpProvider.value,
      profile: { ...profile, idea: description || null },
    })
    catalogRevision.value++
    if (planText.value !== basePlan) throw new Error(t('The builder changed while the request was running. Send it again to apply changes to the latest plan.'))
    const updated = Boolean(projectDescription.value) || analyzedProject.value
    if (captureInput) {
      generationInput.value = {
        prompt: description,
        answers: discoveryAnswers,
        useDefaults: skipDiscovery,
        basePlan: analyzedProject.value ? currentPlan : undefined,
      }
    }
    projectDescription.value = result.projectBrief?.trim() || description
    if (!projectTitle.value && result.sessionTitle) projectTitle.value = result.sessionTitle
    if (refresh || result.hookChecks) hookChecks.value = result.hookChecks ?? []
    discovery.value = undefined
    planCatalogPaths = paths
    builderPaths.value = paths
    project.value = {
      plan: result.candidate.plan, notes: result.candidate.assumptions, offline: false,
    }
    planText.value = JSON.stringify(result.candidate.plan, null, 2)
    messages.value.push({ role: 'assistant', text: [refresh ? t('Structure refreshed.') : updated ? t('Plan updated.') : t('Initial setup added to the builder.'), ...(analyzedProject.value ? result.candidate.assumptions : [])].join('\n') })
    selectedModule.value = undefined
    screen.value = 'builder'
  } catch (cause) {
    if (!refresh) idea.value = submitted
    messages.value.push({ role: 'error', text: String(cause) })
  } finally {
    inspecting.value = false
    if (!refresh) {
      await nextTick()
      const input = chatInput.value
      const active = document.activeElement
      if (input && screen.value === 'builder' && !promptDialog.value?.open && !instructionDialog.value?.open
        && (!active || active === document.body || input.form?.contains(active) || active.closest('.initial-prompt'))) {
        input.focus({ preventScroll: true })
      }
    }
  }
}
</script>

<template>
  <main v-if="restoringConnection" class="startup-screen" aria-busy="true">
    <p role="status">
      <LoaderCircle :size="18" class="motion-safe:animate-spin" aria-hidden="true" />
      {{ t('Restoring connection…') }}
    </p>
  </main>
  <main v-else class="min-h-screen text-zinc-900" :class="{ 'auth-page': screen === 'connect', 'editor-shell': screen !== 'connect', 'builder-shell': screen === 'builder' }">
    <div v-if="screen === 'connect' || screen === 'projects'" class="language-switch" role="group" :aria-label="t('Language')">
      <button type="button" lang="ru" aria-label="Русский" :aria-pressed="language === 'ru'" @click="setLanguage('ru')">RU</button>
      <button type="button" lang="en" aria-label="English" :aria-pressed="language === 'en'" @click="setLanguage('en')">EN</button>
    </div>
    <nav v-if="screen !== 'connect'" class="editor-rail" :aria-label="t('Project navigation')">
      <button v-if="screen !== 'projects'" type="button" :title="t('Back to projects')" :aria-label="t('Back to projects')" :disabled="inspecting || deliveryBusy || openingProject" @click="showProjects"><ArrowLeft :size="19" aria-hidden="true" /></button>
      <button type="button" class="rail-settings" :title="t('Settings')" :aria-label="t('Settings')" :aria-current="screen === 'settings' ? 'page' : undefined" @click="screen = 'settings'"><Settings :size="19" aria-hidden="true" /></button>
      <button type="button" :title="t('Sign out')" :aria-label="t('Sign out')" :disabled="!canSignOut" @click="signOut"><LogOut :size="19" aria-hidden="true" /></button>
    </nav>
    <p v-if="signOutError" role="alert" class="sign-out-error">{{ t('Could not sign out.') }} {{ signOutError }}</p>
    <div class="app-content" :class="{ 'page-content': screen === 'settings', 'projects-layout': screen === 'projects' }">
      <aside v-if="screen === 'projects'" class="history-sidebar" :aria-label="t('History')">
        <h2 class="panel-heading">{{ t('History') }}</h2>
        <p v-if="!history.length" class="chat-empty">{{ t('No projects in history') }}</p>
        <div v-for="item in history" :key="item.id" class="history-row">
          <button type="button" class="history-project" :title="item.name" :disabled="openingProject" @click="openHistory(item)">
            <span class="block truncate">{{ item.name }}</span>
          </button>
          <button type="button" class="history-remove" :disabled="openingProject" :title="t('Remove')"
            :aria-label="t('Remove {name} from history', { name: item.name })" @click="removeHistory(item.id)">
            <Trash2 :size="17" aria-hidden="true" />
          </button>
        </div>
      </aside>
      <div class="min-w-0" :class="{ 'editor-content': screen === 'builder' }">
      <p v-if="workspaceStatus" role="status" class="mb-3 text-amber-800">{{ workspaceStatus }}</p>
      <section v-if="screen === 'projects'" class="projects-home">
        <h1 class="text-2xl font-semibold">{{ t("What's next?") }}</h1>
        <div class="flex flex-wrap gap-3">
          <button type="button" class="auth-button auth-button-primary" :disabled="openingProject" @click="newProject">{{ t("New project") }}</button>
          <button type="button" class="auth-button" :disabled="openingProject" @click="openExistingProject">{{ t("Analyze existing project") }}</button>
        </div>
      </section>
      <h1 v-if="screen === 'connect'" class="text-3xl font-semibold">Prelude</h1>
      <label v-if="screen === 'connect'" class="mt-6 grid gap-2">
        {{ t("Connection method") }}
        <select v-model="activeConnection" class="rounded bg-zinc-100 p-2" :disabled="chatgptBusy || configuring">
          <option value="chatgpt">ChatGPT</option>
          <option value="api">API</option>
          <option v-for="(label, provider) in acpProviders" :key="provider" :value="provider">{{ label }}</option>
        </select>
      </label>
      <fieldset v-if="(screen === 'connect' || screen === 'settings') && acpProvider" class="mt-8 grid gap-3 rounded-lg border border-zinc-200 p-4" :disabled="configuring || inspecting">
        <legend class="font-semibold">{{ acpProviders[acpProvider] }}</legend>
        <p>{{ t('Uses the official CLI installed on this computer.') }}</p>
        <div class="flex flex-wrap gap-3">
          <button class="auth-button auth-button-primary" type="button" @click="updateAcpConnection(acpProvider, 'login')">{{ t('Open sign-in') }}</button>
          <button class="auth-button" type="button" @click="updateAcpConnection(acpProvider, 'check')">{{ t('Test connection') }}</button>
        </div>
        <p v-if="configuring || acpStatus" role="status">{{ configuring ? t('Checking connection…') : acpStatus }}</p>
      </fieldset>
      <fieldset v-if="(screen === 'connect' || screen === 'settings') && activeConnection === 'chatgpt'" class="mt-8 grid gap-3 rounded-lg border border-zinc-200 p-4" :disabled="chatgptBusy || inspecting">
        <legend class="font-semibold">ChatGPT</legend>
        <div class="flex flex-wrap gap-4">
          <button v-if="!chatgptConnected" class="auth-button auth-button-primary" type="button" @click="updateChatGpt('connect')">{{ t("Continue with ChatGPT") }}</button>
          <button v-else class="auth-button" type="button" @click="updateChatGpt('disconnect')">{{ t("Disconnect account") }}</button>
        </div>
        <button v-if="screen === 'settings'" class="justify-self-start underline" type="button" :disabled="!chatgptConnected || !chatgptModel.trim()" @click="updateChatGpt('test')">{{ t("Test connection") }}</button>
        <p v-if="chatgptBusy || chatgptStatus" role="status">{{ chatgptStatus || t('Waiting for confirmation…') }}</p>
      </fieldset>
      <section v-if="(screen === 'connect' || screen === 'settings') && activeConnection === 'api'" class="mt-8 grid gap-3 rounded-lg border border-zinc-200 p-4">
        <h2 class="font-semibold">API</h2>
        <fieldset class="grid gap-3" :disabled="configuring || inspecting">
          <label class="grid gap-2">
            {{ t("HTTPS endpoint") }}
            <input v-model="apiEndpoint" type="url" list="api-endpoints" placeholder="https://provider.example/v1/chat/completions" :disabled="apiConnected" />
          </label>
          <datalist id="api-endpoints">
            <option v-for="preset in endpointPresets" :key="preset.url" :value="preset.url" :label="preset.name" />
          </datalist>
          <button v-if="apiConnected" type="button" class="justify-self-start underline" @click="checkApiModel">{{ t("Test model") }}</button>
          <p v-if="isOpenRouter && apiModel.trim()" class="text-sm text-zinc-700" role="status">
            {{ apiPricePending ? t('Loading OpenRouter pricing…') : apiPrice ? t('{input} USD per input token · {output} USD per output token', { input: apiPrice.input, output: apiPrice.output }) : t('Per-token pricing is unavailable.') }}
          </p>
          <label class="grid gap-2">{{ t("Model ID (optional)") }}<input v-model="apiModelInput" type="text" :placeholder="t('Provider model ID')" autocomplete="off" spellcheck="false" :disabled="apiConnected" /></label>
          <label>{{ t("API key") }}<input v-model="apiKey" type="password" autocomplete="off" /></label>
          <div class="flex gap-4">
            <button class="auth-button auth-button-primary" type="button" :disabled="!apiKey.trim() || !apiEndpoint.trim()" @click="configureApi()">{{ t("Connect") }}</button>
            <button v-if="apiConnected" class="auth-button" type="button" @click="configureApi(true)">{{ t("Disconnect") }}</button>
          </div>
        </fieldset>
        <p v-if="apiStatus" role="status">{{ apiStatus }}</p>
      </section>
      <section v-if="screen === 'settings'" class="mt-8 grid gap-3">
        <h2 class="text-xl font-semibold">{{ t('Skill and tool preferences') }}</h2>
        <p class="text-sm">{{ t('Use catalog IDs. The planning field guides selection, and instructions controls when the exported agent should use a selected skill.') }}</p>
        <textarea v-model="modulePreferencesText" rows="12" class="font-mono text-sm" spellcheck="false"
          :aria-label="t('Module preferences JSON')" :disabled="modulePreferencesBusy" />
        <div class="flex flex-wrap gap-3">
          <button type="button" class="auth-button auth-button-primary" :disabled="modulePreferencesBusy || !modulePreferencesText.trim()" @click="editModulePreferences('save')">{{ t('Save preferences') }}</button>
          <button type="button" class="auth-button" :disabled="modulePreferencesBusy" @click="editModulePreferences('defaults')">{{ t('Load defaults') }}</button>
        </div>
        <p v-if="modulePreferencesStatus" role="status">{{ modulePreferencesStatus }}</p>
        <p v-if="modulePreferencesError" class="text-red-700" role="alert">{{ modulePreferencesError }}</p>
      </section>
      <p v-if="error" class="mt-5 text-red-700" role="alert">{{ error }}</p>
      <section v-if="project && screen === 'builder'" class="grid editor-workspace">
        <header v-if="screen === 'builder'" class="editor-toolbar">
          <h1 class="project-path">{{ projectTitle || t("Untitled project") }}</h1>
          <div class="editor-actions">
            <button type="button" :disabled="inspecting" @click="yamlInput?.click()">{{ t("Import prelude.yaml") }}</button>
            <input ref="yamlInput" hidden type="file" accept=".yaml,.yml" @change="openYaml" />
            <button type="button" @click="saveYaml">{{ t("Save prelude.yaml") }}</button>
            <button type="button" :disabled="!canRefreshStructure" @click="inspectProject('refresh')">{{ t("Refresh structure") }}</button>
          </div>
        </header>
        <div v-if="screen === 'builder'" class="editor-layout">
          <form class="editor-chat" @submit.prevent="inspectProject()">
            <h2 class="panel-heading"><MessageSquare class="chat-mark" :size="16" aria-hidden="true" /> {{ t("Project chat") }}</h2>
            <div ref="chatLog" class="editor-chat-log" role="log">
              <p v-if="!messages.length && !inspecting" class="chat-empty">{{ t("Describe your project to get a suggested starting setup.") }}</p>
              <p v-for="(message, index) in messages" :key="index" class="whitespace-pre-wrap break-words rounded-lg p-2 text-sm" :class="message.role === 'error' ? 'bg-red-50 text-red-800' : message.role === 'user' ? 'bg-zinc-100' : 'text-zinc-700'" :role="message.role === 'error' ? 'alert' : undefined">{{ message.text }}</p>
              <div v-if="inspecting" class="chat-waiting" role="status">
                <LoaderCircle class="motion-safe:animate-spin" :size="16" aria-hidden="true" />
                <span>{{ t("Waiting for a response…") }}</span>
              </div>
            </div>
            <div class="editor-composer">
            <textarea ref="chatInput" v-model="idea" rows="3" :aria-label="t('Project prompt')" @keydown.enter="handlePromptEnter" :placeholder="discovery ? t('Answer the question or ask AI to choose defaults…') : projectDescription ? t('Ask AI to change the plan…') : t('Describe your project…')" :disabled="inspecting" />
            <div class="composer-actions">
              <select v-model="chatModel" :aria-label="t('Chat model')" class="w-full min-w-0 rounded bg-zinc-100 p-2" :disabled="loadingModels || inspecting">
                <option value="" disabled>{{ loadingModels ? t('Loading models…') : t('Select a model') }}</option>
                <option v-for="model in modelOptions" :key="model.id" :value="model.id">{{ model.name }}</option>
              </select>
              <button class="rounded bg-zinc-800 px-3 py-2 text-zinc-950 disabled:opacity-50"
                :disabled="!canSendPrompt">{{ inspecting ? t('Working…') : t('Send') }}</button>
            </div>
            <button v-if="discovery" type="button" class="discovery-defaults" :disabled="!canRequestPlan" @click="inspectProject('defaults')">
              {{ t('Choose defaults') }}
            </button>
            </div>
            <p v-if="modelError" class="text-red-700" role="alert">{{ modelError }}</p>
            <button v-if="modelError" class="text-left underline" type="button" @click="loadModels()">{{ t("Retry loading") }}</button>
          </form>
          <div class="editor-canvas-workspace">
            <div class="editor-canvas-panel" :inert="deliveryBusy">
            <PlanBuilder :plan-text="planText" :local-skills="localSkills" :hook-checks="hookChecks" :detected-ids="detectedIds" :paths="builderPaths" :catalog-revision="catalogRevision" :selected-id="selectedModule?.id" :declared-dependency-count="declaredDependencies.length" @select-module="selectedModule = $event" @open-source="openModuleSource" @canvas-elements="hasCanvasElements = $event" />
            </div>
            <div class="build-overlay">
              <p v-if="deliveryError" class="build-status text-red-800" role="alert">{{ deliveryError }}</p>
              <button v-if="hasCanvasElements" type="button" class="auth-button auth-button-primary build-project-button" :disabled="inspecting || checkingPlan || deliveryBusy" @click="prepareInstructions">
                {{ t('Export') }}
              </button>
            </div>
          </div>
          <aside :aria-label="t('Module details')" class="editor-inspector" :inert="deliveryBusy">
            <template v-if="selectedModule">
              <div class="inspector-head">
                <h2 class="font-semibold">{{ selectedModule.name }}</h2>
                <button type="button" :aria-label="t('Close details')" @click="selectedModule = undefined"><X :size="17" aria-hidden="true" /></button>
              </div>
              <p v-if="selectedModule.version && selectedModule.version.toLowerCase() !== 'unknown'" class="text-sm text-zinc-600">{{ selectedModule.version }}</p>
              <a v-if="moduleSource" :href="moduleSource" class="block break-all text-zinc-800 underline"
                @click.prevent="openModuleSource(moduleSource)" @auxclick.prevent>{{ moduleSource }}</a>
              <p v-if="moduleReason" class="mt-3">{{ moduleReason }}</p>
              <p v-if="selectedModule.model" class="mt-3 text-sm">{{ t('Model') }} · {{ selectedModule.model }}</p>
              <p v-if="selectedModule.requires?.length" class="mt-3 text-sm">{{ t('Requires') }} · {{ selectedModule.requires.join(', ') }}</p>
              <p v-if="selectedModule.conflicts?.length" class="mt-3 text-sm">{{ t('Conflicts') }} · {{ selectedModule.conflicts.join(', ') }}</p>
            </template>
            <p v-if="selectedModule?.interaction" class="mt-3 text-sm">{{ selectedModule.interaction }}</p>
            <p v-if="selectedModule?.parallelWith?.length" class="mt-3 text-sm">{{ t('Works in parallel with {roles}', { roles: selectedModule.parallelWith.join(', ') }) }}</p>
          </aside>
        </div>
      </section>
    </div>
    </div>
    <dialog ref="promptDialog" class="build-guide initial-prompt" aria-labelledby="prompt-title" @click="dismissOnBackdrop">
      <form class="grid gap-3" @submit.prevent="startProject">
        <h2 id="prompt-title" class="text-xl font-semibold">{{ t('New project') }}</h2>
        <textarea v-model="newDescription" rows="5" required autofocus @keydown.enter="handlePromptEnter"
          :aria-label="t('Project description')" :placeholder="t('Describe what you want to build…')" />
        <button type="submit" class="auth-button auth-button-primary justify-self-end" :disabled="!newDescription.trim()">{{ t('Continue') }}</button>
      </form>
    </dialog>
    <dialog ref="instructionDialog" class="build-guide export-dialog" aria-labelledby="instruction-title"
      @click="dismissOnBackdrop" @cancel="deliveryBusy && $event.preventDefault()">
      <div class="flex items-center justify-between gap-3">
        <h2 id="instruction-title" class="text-xl font-semibold">{{ t('Export') }}</h2>
        <button type="button" :disabled="deliveryBusy" :aria-label="t('Close')" @click="instructionDialog?.close()"><X :size="18" aria-hidden="true" /></button>
      </div>
      <label class="export-target">
        {{ t('Target agent') }}
        <select v-model="exportTarget" :disabled="deliveryBusy" @change="loadExport">
          <option v-for="target in exportTargets" :key="target.id" :value="target.id">{{ target.name }}</option>
        </select>
      </label>
      <p v-if="deliveryBusy && !activeExport" role="status">{{ t('Preparing preview…') }}</p>
      <p v-if="activeExport?.omitted.length" role="status">{{ t('Unavailable for this agent: {modules}', { modules: activeExport.omitted.join(', ') }) }}</p>
      <details v-if="activeExport?.skillErrors?.length" class="text-sm">
        <summary>{{ t('Some skills could not be included in the archive') }}</summary>
        <ul class="list-disc pl-5"><li v-for="error in activeExport.skillErrors" :key="error">{{ error }}</li></ul>
      </details>
      <p v-if="activeExport?.files['.codex/hooks.json']" class="text-sm">
        {{ t('Included checks: {checks}', { checks: hookChecks.filter(hook => hook.check).map(hook => hook.title || hook.check).join(', ') }) }}
        {{ t('Open the project in the selected agent and approve the hooks before running them.') }}
      </p>
      <p v-if="activeExport && hookChecks.some(hook => !hook.check || !activeExport?.files['.codex/hooks.json'])" class="text-sm">
        {{ t('Some proposed hooks require setup in the selected agent.') }}
      </p>
      <div v-if="activeExport" class="export-editor">
        <nav class="export-tree" :aria-label="t('Export files')">
          <template v-for="row in exportTree" :key="row.path">
            <div v-if="row.folder" class="export-tree-row" :style="{ paddingInlineStart: 8 + row.depth * 14 + 'px' }"><Folder :size="14" aria-hidden="true" />{{ row.name }}</div>
            <button v-else type="button" class="export-tree-row" :title="row.path" :aria-pressed="selectedExportPath === row.path"
              :style="{ paddingInlineStart: 8 + row.depth * 14 + 'px' }" @click="selectedExportPath = row.path"><FileText :size="14" aria-hidden="true" />{{ row.name }}</button>
          </template>
        </nav>
        <label v-if="selectedExportPath" class="export-editor-file">
          <span>{{ selectedExportPath }}</span>
          <textarea :key="selectedExportPath" v-model="activeExport.files[selectedExportPath]" class="instruction-text" :disabled="deliveryBusy" spellcheck="false" />
        </label>
      </div>
      <p v-if="exportError" role="alert" class="text-red-700">{{ exportError }}</p>
      <p v-if="exportStatus" role="status">{{ exportStatus }}</p>
      <div class="instruction-actions">
        <button type="button" class="auth-button auth-button-primary" :disabled="!activeExport || deliveryBusy" @click="saveExport">{{ t('Export') }}</button>
      </div>
    </dialog>
  </main>
</template>

<style scoped>
.startup-screen { display: grid; min-height: 100dvh; place-items: center; background: var(--surface-soft); color: var(--text-muted); }
.startup-screen p { display: flex; align-items: center; gap: 8px; font-size: 14px; }
.build-guide { margin: auto; width: min(52rem, calc(100% - 32px)); max-height: calc(100% - 32px); padding: 24px; border: 1px solid var(--border); border-radius: 14px; background: white; color: var(--text-strong); box-shadow: 0 16px 48px #0002; }
.initial-prompt { max-width: 36rem; }
.build-guide::backdrop { background: #18181b66; }
.build-guide p { margin-top: 8px; overflow-wrap: anywhere; }
.instruction-text { width: 100%; margin-block: 12px; white-space: pre-wrap; overflow-wrap: anywhere; font-family: monospace; }
.instruction-actions { display: flex; flex-wrap: wrap; justify-content: end; gap: 8px; margin-top: 12px; }
.export-dialog { width: min(66rem, calc(100% - 32px)); }
.export-target { display: grid; gap: 5px; max-width: 19rem; margin-block: 12px; }
.export-editor { display: grid; grid-template-columns: minmax(125px, 28%) minmax(0, 1fr); min-height: 0; height: clamp(220px, 49vh, 480px); gap: 10px; }
.export-tree, .export-editor-file { min-width: 0; min-height: 0; border: 1px solid var(--border); border-radius: 8px; overflow: auto; }
.export-tree { padding: 4px; }
.export-tree-row { display: flex; align-items: center; gap: 6px; width: 100%; min-height: 30px; padding: 4px 6px; border-radius: 5px; overflow-wrap: anywhere; text-align: left; font-size: 13px; }
.export-tree-row svg { flex: none; }
button.export-tree-row:hover, .export-tree-row[aria-pressed="true"] { background: var(--surface-selected); }
.export-editor-file { display: grid; grid-template-rows: auto minmax(0, 1fr); }
.export-editor-file span { padding: 8px; border-bottom: 1px solid var(--border); overflow-wrap: anywhere; font-family: monospace; font-size: 12px; }
.export-editor-file .instruction-text { margin: 0; border: 0; border-radius: 0; padding: 10px; }
.auth-page { position: relative; padding: 56px 12px 16px; background: var(--surface-soft); }
.language-switch { position: absolute; top: 12px; right: 16px; z-index: 1; display: flex; gap: 2px; padding: 3px; border: 1px solid var(--border); border-radius: 9px; background: white; }
.language-switch button { min-width: 33px; padding: 4px 5px; border-radius: 5px; color: var(--text-muted); font-size: 12px; font-weight: 600; }
.language-switch button[aria-pressed="true"] { background: var(--surface-selected); color: var(--text-strong); }
.auth-page .app-content { max-width: 36rem; margin: auto; }
.app-content { min-width: 0; }
.page-content { padding: 16px clamp(16px, 3vw, 36px); }
.editor-shell { display: grid; grid-template-columns: 52px minmax(0, 1fr); min-height: 100vh; max-width: none; position: relative; }
.builder-shell { height: 100dvh; min-height: 0; grid-template-rows: minmax(0, 1fr); overflow: hidden; }
.builder-shell .app-content { display: flex; min-height: 0; }
.builder-shell .editor-rail { height: auto; }
.editor-rail { background: var(--surface-soft); color: var(--text-medium); border-right: 1px solid var(--border); display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 10px 5px; position: sticky; top: 0; height: 100vh; }
.editor-rail button { display: grid; place-items: center; padding: 0; flex-shrink: 0; width: 35px; height: 35px; border-radius: 8px; }
.editor-rail button[aria-current="page"] { background: var(--surface-selected); }
.editor-rail button:hover { background: var(--surface-hover); }
.rail-settings { margin-top: auto; }
.editor-rail button:disabled { opacity: .5; }
.sign-out-error { position: absolute; z-index: 5; left: 60px; bottom: 12px; max-width: min(30rem, calc(100% - 72px)); padding: 8px 12px; border: 1px solid var(--border); border-radius: 8px; background: white; color: #982f2f; white-space: pre-wrap; }
.auth-button { min-height: 36px; padding: 7px 14px; border: 1px solid var(--border-strong); border-radius: 8px; background: #fff; color: var(--text-strong); font-weight: 600; }
.auth-button:not(:disabled):hover { background: var(--surface-hover); }
.auth-button-primary { background: var(--button-fill); border-color: var(--button-fill); color: #fff; }
.auth-button-primary:not(:disabled):hover { background: var(--button-hover); }
.auth-button:disabled { opacity: .55; }
.editor-content { display: flex; flex: 1; flex-direction: column; min-width: 0; min-height: 0; }
.editor-workspace { flex: 1; min-height: 0; grid-template-rows: auto minmax(0, 1fr); gap: 0; }
.editor-toolbar { min-height: 62px; background: #fff; border-bottom: 1px solid var(--border); display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 8px 16px; }
.projects-home { display: grid; align-content: start; gap: 18px; padding: 56px clamp(20px, 5vw, 52px) clamp(20px, 5vw, 52px); }
.projects-layout { display: grid; grid-template-columns: minmax(180px, 24%) minmax(0, 1fr); }
.history-sidebar { min-width: 0; display: flex; flex-direction: column; gap: 8px; padding: 0 12px 12px; border-right: 1px solid var(--border); background: var(--surface-panel); }
.history-row { display: grid; grid-template-columns: minmax(0, 1fr) 40px; gap: 4px; }
.history-project, .history-remove { padding: 8px; border-radius: 7px; background: var(--surface-canvas); }
.history-project { min-width: 0; text-align: left; }
.history-remove { display: grid; place-items: center; color: var(--text-muted); }
.history-project:not(:disabled):hover, .history-remove:not(:disabled):hover { background: var(--surface-selected); }
.history-remove:not(:disabled):hover { color: #982f2f; }
@media (max-width: 580px) {
  .projects-layout { grid-template-columns: minmax(0, 1fr); }
  .history-sidebar { padding-top: 48px; border-right: 0; border-bottom: 1px solid var(--border); }
  .projects-home { padding-top: 20px; }
}
.directory-field { display: flex; align-items: center; gap: 8px; }
.directory-field input { min-width: 0; }
.directory-field button { flex: none; }
.project-path { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 17px; font-weight: 700; }
.editor-actions { display: flex; gap: 8px; flex-shrink: 0; }
.editor-actions button { border: 1px solid var(--border-strong); border-radius: 7px; padding: 5px 10px; font-size: 13px; }
.editor-actions .primary-action { background: var(--button-fill); color: #fff; border-color: var(--button-fill); }
.editor-actions button:disabled { opacity: .45; }
.editor-layout { display: grid; grid-template-columns: minmax(215px, 24%) minmax(0, 1fr) minmax(185px, 22%); grid-template-rows: minmax(0, 1fr); min-width: 0; min-height: 0; }
.editor-chat, .editor-inspector { background: var(--surface-panel); min-width: 0; min-height: 0; }
.editor-chat { display: flex; flex-direction: column; gap: 10px; border-right: 1px solid var(--border); padding: 0 12px 12px; }
.panel-heading, .inspector-head { display: flex; align-items: center; gap: 8px; min-height: 48px; border-bottom: 1px solid var(--border-subtle); font-size: 14px; font-weight: 700; }
.chat-mark { color: var(--button-fill); }
.editor-chat > :not(.editor-chat-log) { flex-shrink: 0; }
.editor-chat-log { display: grid; align-content: start; flex: 1 1 0; min-height: 0; gap: 8px; overflow: auto; overscroll-behavior: contain; }
.chat-waiting { display: flex; align-items: center; gap: 8px; padding: 8px; color: var(--text-medium); font-size: 13px; }
.chat-empty { padding: 8px 2px; color: var(--text-muted); font-size: 13px; }
.editor-chat-log :deep(p.bg-zinc-100) { background: var(--surface-selected); }
.editor-chat-log :deep(p.text-zinc-700) { color: var(--text-medium); }
.editor-composer { border: 1px solid var(--border-strong); border-radius: 12px; background: #fff; padding: 7px; }
.editor-composer textarea { border: 0; background: transparent; padding: 5px; color: var(--text-strong); }
.composer-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 5px; }
.composer-actions select { background: var(--surface-soft); color: var(--text-medium); font-size: 12px; flex: 1 1 90px; }
.composer-actions button { background: var(--button-fill); color: white; font-size: 12px; }
.discovery-defaults { padding: 5px; color: var(--text-medium); font-size: 12px; }
.discovery-defaults:not(:disabled):hover { text-decoration: underline; }
.discovery-defaults:disabled { opacity: .5; }
.editor-chat [role=alert] { color: #a14031; font-size: 12px; }
.editor-canvas-workspace { position: relative; display: grid; grid-template-rows: minmax(0, 1fr); min-width: 0; min-height: 0; }
.editor-canvas-panel { min-width: 0; min-height: 0; overflow: auto; background: radial-gradient(var(--canvas-dot) 1px, transparent 1px) 0 0 / 18px 18px var(--surface-canvas); }
.build-overlay { position: absolute; inset: auto 12px 16px; display: grid; gap: 8px; justify-items: center; pointer-events: none; }
.build-project-button { pointer-events: auto; box-shadow: 0 5px 15px #18181b38; }
.build-status { max-width: min(100%, 42rem); max-height: 120px; overflow: auto; pointer-events: auto; padding: 8px 12px; border: 1px solid var(--border); border-radius: 8px; background: white; font-size: 13px; white-space: pre-wrap; }
.editor-inspector { border-left: 1px solid var(--border); overflow: auto; padding: 0 12px 12px; font-size: 13px; }
.inspector-head { justify-content: space-between; }
.inspector-head button { font-size: 20px; padding: 4px; }
.editor-inspector .text-zinc-600, .editor-canvas-panel .text-zinc-600 { color: var(--text-muted); }
@media (max-width: 880px) {
  .editor-layout { grid-template-columns: minmax(175px, 31%) minmax(0, 1fr); grid-template-rows: minmax(0, 1fr) fit-content(32%); }
  .editor-chat { grid-row: 1 / -1; }
  .editor-inspector { grid-column: 2; border-top: 1px solid var(--border); }
  .editor-inspector:not(:has(.inspector-head)) { display: none; }
}
@media (max-width: 580px) {
  .editor-layout { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(0, 2fr) minmax(0, 3fr) fit-content(24%); }
  .editor-chat { grid-row: 1; }
  .editor-inspector { grid-column: 1; }
  .editor-toolbar { flex-wrap: wrap; }
}
.nav-button { text-align: left; padding: .5rem .75rem; border-radius: .5rem; background: var(--surface-canvas); }
input:not([type='range']), textarea, select {
  width: 100%;
  border: 1px solid var(--border-strong);
  border-radius: 0.375rem;
  background: #fff;
  padding: 0.5rem 0.75rem;
}

input[type='range'] {
  accent-color: var(--button-fill);
}
</style>
