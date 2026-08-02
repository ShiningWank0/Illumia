// 平文 HTTP の local URL を使う直前に取る明示確認 (docs/12_security.md, SEC-002)。
//
// 信頼できない Wi-Fi では攻撃者が同じ private IP に偽サーバーを立てられるため、
// この確認は保存せず、接続を試みるたびに毎回取る。

export async function confirmInsecureLocal(): Promise<boolean> {
  if (typeof window === 'undefined') return false;
  return window.confirm(
    'ローカル URL は暗号化されていない HTTP です。\n' +
      '信頼できない Wi-Fi では、同じアドレスの偽サーバーにパスワードを送ってしまう危険があります。\n\n' +
      'このネットワークを信頼して接続しますか?'
  );
}
