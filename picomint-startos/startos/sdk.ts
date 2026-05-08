/**
 * Plumbing. DO NOT EDIT.
 *
 * The exported "sdk" const is used throughout this package codebase.
 */
import { StartSdk } from '@start9labs/start-sdk'
import { manifest } from './manifest'

export const sdk = StartSdk.of().withManifest(manifest).build(true)
