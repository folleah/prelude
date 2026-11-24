export type ProjectDependency = {
  ecosystem: string
  package: string
  reason: string
  source?: string
  manifest?: string
}

export type DeclaredDependency = {
  ecosystem: string
  package: string
  manifest: string
  version?: string
  development: boolean
}
