// 単純なトースト通知。アップロード結果などの一時表示に使う。

export type ToastKind = 'info' | 'success' | 'error';

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

let nextId = 1;

class ToastStore {
  items = $state<Toast[]>([]);

  push(message: string, kind: ToastKind = 'info', ttl = 4000): void {
    const id = nextId++;
    this.items = [...this.items, { id, kind, message }];
    setTimeout(() => this.dismiss(id), ttl);
  }

  success(message: string): void {
    this.push(message, 'success');
  }
  error(message: string): void {
    this.push(message, 'error', 6000);
  }

  dismiss(id: number): void {
    this.items = this.items.filter((t) => t.id !== id);
  }
}

export const toasts = new ToastStore();
