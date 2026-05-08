import { FileHelper, z } from '@start9labs/start-sdk'
import { sdk } from '../sdk'

const bitcoindBackend = z.object({
  selection: z.literal('bitcoind'),
  value: z.object({}).strict(),
})

const esploraBackend = z.object({
  selection: z.literal('esplora'),
  value: z.object({
    url: z.string(),
  }),
})

export const shape = z
  .object({
    backend: z.discriminatedUnion('selection', [
      bitcoindBackend,
      esploraBackend,
    ]).optional(),
    uiPassword: z.string().optional(),
  })
  .strip()

export const storeJson = FileHelper.json(
  {
    base: sdk.volumes.main,
    subpath: '/store.json',
  },
  shape,
)
