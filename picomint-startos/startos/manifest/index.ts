import { setupManifest } from '@start9labs/start-sdk'
import { long, short } from './i18n'

export const manifest = setupManifest({
  id: 'picomint',
  title: 'Picomint',
  license: 'MIT',
  packageRepo: 'https://github.com/joschisan/picomint/tree/main/picomint-startos',
  upstreamRepo: 'https://github.com/joschisan/picomint',
  marketingUrl: 'https://github.com/joschisan/picomint',
  donationUrl: null,
  docsUrls: ['https://github.com/joschisan/picomint/blob/main/README.md'],
  description: { short, long },
  volumes: ['main'],
  images: {
    picomint: {
      source: {
        dockerBuild: {
          workdir: '..',
          dockerfile: 'Dockerfile',
        },
      },
      arch: ['x86_64', 'aarch64'],
    },
  },
  alerts: {
    install: null,
    update: null,
    uninstall: null,
    restore: null,
    start: null,
    stop: null,
  },
  dependencies: {
    bitcoind: {
      description: {
        en_US:
          'Provides private, self-hosted blockchain data instead of relying on external Esplora APIs',
      },
      optional: true,
      metadata: {
        title: 'Bitcoin Core',
        icon: 'https://raw.githubusercontent.com/Start9Labs/bitcoin-core-startos/master/icon.svg',
      },
    },
  },
})
