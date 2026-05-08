import { storeJson } from '../fileModels/store.json'
import { sdk } from '../sdk'

const { InputSpec, Value, Variants } = sdk

const passwordPattern = {
  regex: '^[a-zA-Z0-9_]+$',
  description: 'Must be alphanumeric (can contain underscore).',
}

const bitcoindSpec = InputSpec.of({})

const esploraSpec = InputSpec.of({
  url: Value.text({
    name: 'Esplora API URL',
    description:
      'The URL of the Esplora API to use (e.g., https://mempool.space/api).',
    required: true,
    default: 'https://mempool.space/api',
    masked: false,
    patterns: [
      {
        regex: '^https?://.*',
        description: 'Must be a valid HTTP(S) URL.',
      },
    ],
  }),
})

export const inputSpec = InputSpec.of({
  backend: Value.union({
    name: 'Bitcoin Backend',
    description: 'Choose how Picomint connects to the Bitcoin network.',
    default: 'bitcoind',
    variants: Variants.of({
      bitcoind: {
        name: 'Bitcoin Core (recommended — RPC credentials are auto-provisioned via the bitcoind cookie file)',
        spec: bitcoindSpec,
      },
      esplora: {
        name: 'Esplora',
        spec: esploraSpec,
      },
    }),
  }),
  uiPassword: Value.text({
    name: 'UI Password',
    description:
      'Password for the dashboard UI. Required for securing access to your guardian.',
    required: true,
    default: { charset: 'a-z,A-Z,0-9', len: 20 },
    masked: true,
    patterns: [passwordPattern],
    generate: { charset: 'a-z,A-Z,0-9', len: 20 },
  }),
})

export const config = sdk.Action.withInput(
  // id
  'config',

  // metadata
  async ({ effects }) => ({
    name: 'Configure',
    description: 'Configure Picomint guardian settings.',
    warning: null,
    allowedStatuses: 'any',
    group: null,
    visibility: 'enabled',
  }),

  // input spec
  inputSpec,

  // pre-fill from store
  async ({ effects }) => storeJson.read().once(),

  // execute: persist to store
  async ({ effects, input }) => {
    await storeJson.merge(effects, input)
  },
)
