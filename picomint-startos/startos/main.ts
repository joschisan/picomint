import { FileHelper } from '@start9labs/start-sdk'
import { storeJson } from './fileModels/store.json'
import { sdk } from './sdk'
import { dataDir, defaultRustLog, uiPort } from './utils'

const BITCOIND_MOUNTPOINT = '/mnt/bitcoind'

export const main = sdk.setupMain(async ({ effects }) => {
  const store = await storeJson.read().const(effects)
  if (!store) {
    throw new Error('Store not initialized')
  }

  const { backend, uiPassword } = store

  if (!backend) {
    throw new Error(
      'Picomint is not configured. Run the "Configure" action and choose a Bitcoin backend.',
    )
  }
  if (!uiPassword) {
    throw new Error(
      'Picomint is not configured. Run the "Configure" action and set a UI password.',
    )
  }

  let mounts = sdk.Mounts.of().mountVolume({
    volumeId: 'main',
    subpath: null,
    mountpoint: dataDir,
    readonly: false,
  })
  if (backend.selection === 'bitcoind') {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    mounts = mounts.mountDependency<any>({
      dependencyId: 'bitcoind',
      volumeId: 'main',
      subpath: null,
      mountpoint: BITCOIND_MOUNTPOINT,
      readonly: true,
    })
  }

  const subcontainer = await sdk.SubContainer.of(
    effects,
    { imageId: 'picomint' },
    mounts,
    'picomint-sub',
  )

  const env: Record<string, string> = {
    DATA_DIR: dataDir,
    BITCOIN_NETWORK: 'bitcoin',
    UI_ADDR: `0.0.0.0:${uiPort}`,
    UI_PASSWORD: uiPassword,
    RUST_LOG: defaultRustLog,
  }

  if (backend.selection === 'bitcoind') {
    // bitcoind writes a per-session cookie at <datadir>/.cookie containing
    // "__cookie__:<hex>". Read it reactively so picomint-server-daemon
    // restarts with fresh credentials when bitcoind rotates the cookie
    // (e.g. across bitcoind restarts).
    const cookie = await FileHelper.string(
      `${subcontainer.rootfs}${BITCOIND_MOUNTPOINT}/.cookie`,
    )
      .read()
      .const(effects)
    if (!cookie) {
      throw new Error(
        'Bitcoin Core cookie not yet available — wait for bitcoind to finish starting.',
      )
    }
    const [user, password] = cookie.trim().split(':', 2)
    if (!user || !password) {
      throw new Error('Malformed bitcoind cookie file')
    }
    env.BITCOIND_URL = 'http://bitcoind.embassy:8332'
    env.BITCOIND_USERNAME = user
    env.BITCOIND_PASSWORD = password
  } else {
    env.ESPLORA_URL = backend.value.url
  }

  return sdk.Daemons.of(effects).addDaemon('primary', {
    subcontainer,
    exec: { command: ['picomint-server-daemon'], env },
    ready: {
      display: 'Web Interface',
      fn: () =>
        sdk.healthCheck.checkPortListening(effects, uiPort, {
          successMessage: 'Guardian dashboard is available',
          errorMessage: 'Guardian dashboard is unreachable',
        }),
    },
    requires: [],
  })
})
