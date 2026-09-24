import { api } from '../api';
import { fixtureStatus, fixtureWizard } from '../fixtures';
import type { SystemStatus, WizardStatus } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

let status = $state<SystemStatus | null>(null);
let wizard = $state<WizardStatus | null>(null);
let connected = $state(true);

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

export async function loadStatus(): Promise<void> {
  status = isMock ? fixtureStatus : await api.status();
}

export async function loadWizard(): Promise<void> {
  wizard = isMock ? fixtureWizard : await api.wizard();
}

export function applyStatus(next: SystemStatus): void {
  status = next;
}
