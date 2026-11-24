export type HookCheck = 'test' | 'lint' | 'typecheck'

export type PlannedHook = {
  title: string
  description: string
  check?: HookCheck
}
