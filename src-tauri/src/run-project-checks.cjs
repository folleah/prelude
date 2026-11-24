const { spawnSync } = require('node:child_process')
const { readFileSync } = require('node:fs')
const { resolve } = require('node:path')

const [, , manager, ...checks] = process.argv
const root = resolve(__dirname, '../..')
const report = systemMessage => process.stdout.write(JSON.stringify({ systemMessage }))
if (!['npm', 'pnpm', 'yarn', 'bun'].includes(manager)
  || !checks.length || !checks.every(check => ['test', 'lint', 'typecheck'].includes(check))) {
  report('Prelude checks skipped: unsupported package manager or script.')
  return
}
let scripts
try {
  scripts = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8'))?.scripts
} catch {
  report('Prelude checks skipped: no readable package.json.')
  return
}

const windows = process.platform === 'win32'
const results = checks.map(check => {
  if (typeof scripts?.[check] !== 'string' || !scripts[check].trim()) {
    return `${check}: skipped (script missing)`
  }
  const result = spawnSync(windows ? 'cmd.exe' : manager,
    windows ? ['/d', '/c', `${manager} run ${check}`] : ['run', check],
    { cwd: root, timeout: 30000, stdio: 'ignore', windowsHide: true })
  return `${check}: ${result.error || result.status !== 0 ? 'failed or timed out; run it directly to inspect the output' : 'passed'}`
})
report(`Prelude checks — ${results.join('; ')}.`)
