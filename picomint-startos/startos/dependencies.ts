import { storeJson } from './fileModels/store.json'
import { sdk } from './sdk'

export const setDependencies = sdk.setupDependencies(async ({ effects }) => {
  const store = await storeJson.read().const(effects)
  if (store?.backend?.selection === 'bitcoind') {
    return {
      bitcoind: {
        kind: 'running',
        versionRange: '>=31.0.0:0 <32.0.0:0',
        healthChecks: [],
      },
    }
  }
  return {}
})
