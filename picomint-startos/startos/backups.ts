import { sdk } from './sdk'

// picomint-server-daemon writes its full guardian config to <data_dir>/config.json
// (atomic write-then-rename, written every startup) and auto-recovers from the
// same file when the daemon boots against a fresh data dir. We sync only that
// file: the live redb is regenerable from federation peers, so capturing it
// adds size and consistency risk for no gain. Restoring this file alone is
// sufficient to bring a guardian back — no in-app export/import dance.
export const { createBackup, restoreInit } = sdk.setupBackups(
  async ({ effects }) =>
    sdk.Backups.ofSyncs({
      dataPath: '/media/startos/volumes/main/config.json',
      backupPath: '/media/startos/backup/config.json',
    }),
)
