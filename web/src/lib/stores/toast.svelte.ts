export interface ToastMessage {
  id: number;
  text: string;
}

let toasts = $state<ToastMessage[]>([]);
let nextId = 1;

export function getToasts(): ToastMessage[] {
  return toasts;
}

export function showToast(text: string): void {
  const id = nextId++;
  toasts = [...toasts, { id, text }];
  setTimeout(() => dismissToast(id), 5000);
}

export function dismissToast(id: number): void {
  toasts = toasts.filter((t) => t.id !== id);
}
