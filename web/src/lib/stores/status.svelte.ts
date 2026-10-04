import { api } from '../api';
import type { SystemStatus, WizardStatus } from '../types';

let status = $state<SystemStatus | null>(null);
let wizard = $state<WizardStatus | null>(null);
let connected = $state(true);
let unauthorized = $state(false);

export function getStatus(): SystemStatus | null {
  return status;
}

export function getWizard(): WizardStatus | null {
  return wizard;
}

export function isConnected(): boolean {
  return connected;
}

export function setConnected(value: boolean): void {
  connected = value;
}

export function isUnauthorized(): boolean {
  return unauthorized;
}

export function setUnauthorized(value: boolean): void {
  unauthorized = value;
}

export async function loadStatus(): Promise<void> {
  status = await api.status();
}

export async function loadWizard(): Promise<void> {
  wizard = await api.wizard();
}

export function applyStatus(next: SystemStatus): void {
  status = next;
}
