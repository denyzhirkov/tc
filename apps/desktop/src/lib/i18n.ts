// Tiny reactive i18n helper.
//
// Usage:
//   import { t } from "./i18n";
//   <span>{t("settings.audio")}</span>
//
// Language is driven by `state.status.language` (see store / app_status).
// Any component that calls `t()` re-renders automatically when the language
// changes, since `t` reads from the Solid store.

import { state } from "./store";

type Dict = Record<string, string>;

const en: Dict = {
  // Settings sections
  "settings.audio": "audio",
  "settings.input": "input",
  "settings.output": "output",
  "settings.input_device": "input device",
  "settings.output_device": "output device",
  "settings.input_gain": "input gain",
  "settings.output_volume": "output volume",
  "settings.voice_mode": "voice mode",
  "settings.vad_level": "VAD level",
  "settings.test_signal": "test signal",
  "settings.sound_check": "sound check",
  "settings.sound_check_start": "test mic + connection",
  "settings.sound_check_hint": "speak — you'll hear yourself back through the server",
  "settings.sound_check_recording": "listening… speak now ({secs}s)",
  "settings.sound_check_in_channel": "leave the channel first",
  "settings.sound_check_disconnected": "connect to a server first",
  "settings.sound_check_ok": "✓ heard you back — mic, output and connection all work",
  "settings.sound_check_silent": "no audio came back — were you silent? (VAD may have gated it)",
  "settings.sound_check_no_conn": "no round-trip — server unreachable over UDP",
  "settings.sound_check_no_mic": "no microphone detected",
  "settings.denoise": "noise suppression",
  "settings.denoise_hint":
    "RNNoise cleans keyboard / fan / background off your mic before sending",
  "settings.paranoid": "paranoid mode",
  "settings.paranoid_hint":
    "constant packet rate hides who's speaking when from the server (more bandwidth; not byte-exact size)",
  "settings.identity": "identity",
  "settings.fingerprint": "fingerprint",
  "settings.pubkey": "public key",
  "settings.app": "app",
  "settings.notifications": "notifications",
  "settings.autostart": "launch on login",
  "settings.close_to_tray": "close to tray",
  "settings.language": "language",
  "settings.hotkeys": "hotkeys",
  "settings.about": "about",

  // Sidebar
  "sidebar.channels": "channels",
  "sidebar.dms": "direct messages",
  "sidebar.no_channels": "no channels — try /list",
  "sidebar.no_dms": "no DMs yet",
  "sidebar.add_server": "add server…",
  "sidebar.no_servers": "no known servers",
  "sidebar.jump_to": "jump to…",

  // Connection / system messages
  "conn.connecting": "connecting...",
  "conn.connected": "connected to {server}",
  "conn.disconnected": "disconnected",
  "conn.reconnecting": "reconnecting (attempt {attempt}, in {delay}s)…",
  "log.joined_channel": "joined #{channel} ({count} peers)",
  "log.left_channel": "left channel",
  "log.peer_joined": "* {name} joined",
  "log.peer_left": "* {name} left",
  "log.name_changed": "* {old} → {new}",
  "log.dm_from": "(dm from {name}) {text}",
  "log.invite_failed": "invite failed: {error}",
  "log.invite_declined": "invite declined",
  "log.error": "error: {message}",

  // tc:// invite confirmation
  "invite.title": "connect to unknown server?",
  "invite.body":
    "this link points to a server you haven't connected to before. connecting reveals your IP address and public key to it.",
  "invite.connect": "connect",
  "invite.cancel": "cancel",

  // Hot-plugged audio device prompt
  "device.added_input": "new microphone: {name}",
  "device.added_output": "new audio output: {name}",
  "device.use_now": "use it now?",
  "device.yes": "yes",
  "device.no": "no",
  "device.switched": "switched to {name}",

  // Degraded call chips
  "voice.no_mic": "no mic",
  "voice.no_output": "no audio out",

  // Voice mode names
  "voice_mode.open": "open mic",
  "voice_mode.vad": "voice activation",
  "voice_mode.ptt": "push to talk",
  "voice.connected": "connected",
  "voice.no_rx": "no rx — registering\u2026",
  "voice.leave_call": "leave call",
  "voice.toggle_mode": "mode: {mode} (click vad ↔ ptt)",

  // Buttons / shared
  "common.mute": "mute",
  "common.unmute": "unmute",
  "common.disconnect": "disconnect",
  "common.connect": "connect",
  "common.cancel": "cancel",
  "common.on": "on",
  "common.off": "off",
  "common.add": "add",
  "common.copy": "copy",
  "common.close": "close",
  "common.appearance": "appearance",
  "common.nickname": "nickname",
  "common.identicon": "identicon",
  "common.none": "(none)",
  "common.test": "test",

  // Settings extras
  "settings.title": "settings",
  "settings.esc_close": "esc · close",
  "settings.notifications_desc": "OS notifications when window unfocused",
  "settings.tray_desc": "hide to tray on close (instead of quit)",
  "settings.autostart_desc": "launch on system login",
  "settings.hotkey_format": "format: e.g. {example}, CommandOrControl+Shift+M, F19",
};

const ru: Dict = {
  "settings.audio": "звук",
  "settings.input": "микрофон",
  "settings.output": "динамики",
  "settings.input_device": "устройство ввода",
  "settings.output_device": "устройство вывода",
  "settings.input_gain": "усиление микрофона",
  "settings.output_volume": "громкость",
  "settings.voice_mode": "режим голоса",
  "settings.vad_level": "уровень VAD",
  "settings.test_signal": "тестовый сигнал",
  "settings.sound_check": "проверка звука",
  "settings.sound_check_start": "проверить микрофон и связь",
  "settings.sound_check_hint": "говорите — вы услышите себя обратно через сервер",
  "settings.sound_check_recording": "слушаю… говорите ({secs}с)",
  "settings.sound_check_in_channel": "сначала выйдите из канала",
  "settings.sound_check_disconnected": "сначала подключитесь к серверу",
  "settings.sound_check_ok": "✓ вы услышали себя — микрофон, вывод и связь работают",
  "settings.sound_check_silent": "звук не вернулся — вы молчали? (мог сработать VAD)",
  "settings.sound_check_no_conn": "нет round-trip — сервер недоступен по UDP",
  "settings.sound_check_no_mic": "микрофон не обнаружен",
  "settings.denoise": "шумоподавление",
  "settings.denoise_hint":
    "RNNoise убирает клавиатуру / вентилятор / фон с микрофона до отправки",
  "settings.paranoid": "параноик-режим",
  "settings.paranoid_hint":
    "постоянная частота пакетов скрывает от сервера, кто и когда говорит (больше трафика; размер не байт-в-байт)",
  "settings.identity": "личность",
  "settings.fingerprint": "отпечаток",
  "settings.pubkey": "публичный ключ",
  "settings.app": "приложение",
  "settings.notifications": "уведомления",
  "settings.autostart": "запуск при входе",
  "settings.close_to_tray": "сворачивать в трей",
  "settings.language": "язык",
  "settings.hotkeys": "горячие клавиши",
  "settings.about": "о программе",

  "sidebar.channels": "каналы",
  "sidebar.dms": "личные сообщения",
  "sidebar.no_channels": "нет каналов — попробуй /list",
  "sidebar.no_dms": "нет личных сообщений",
  "sidebar.add_server": "добавить сервер…",
  "sidebar.no_servers": "нет известных серверов",
  "sidebar.jump_to": "перейти к…",

  "conn.connecting": "подключение...",
  "conn.connected": "подключено к {server}",
  "conn.disconnected": "отключено",
  "conn.reconnecting": "переподключение (попытка {attempt}, через {delay}с)…",
  "log.joined_channel": "вошли в #{channel} ({count} участников)",
  "log.left_channel": "вышли из канала",
  "log.peer_joined": "* {name} зашёл",
  "log.peer_left": "* {name} вышел",
  "log.name_changed": "* {old} → {new}",
  "log.dm_from": "(лс от {name}) {text}",
  "log.invite_failed": "ошибка приглашения: {error}",
  "log.invite_declined": "приглашение отклонено",
  "log.error": "ошибка: {message}",

  "invite.title": "подключиться к неизвестному серверу?",
  "invite.body":
    "эта ссылка ведёт на сервер, к которому вы ещё не подключались. при подключении он узнает ваш IP-адрес и публичный ключ.",
  "invite.connect": "подключиться",
  "invite.cancel": "отмена",

  "device.added_input": "новый микрофон: {name}",
  "device.added_output": "новое аудио-устройство вывода: {name}",
  "device.use_now": "использовать сейчас?",
  "device.yes": "да",
  "device.no": "нет",
  "device.switched": "переключено на {name}",

  "voice.no_mic": "нет микрофона",
  "voice.no_output": "нет вывода звука",

  "voice_mode.open": "открытый микрофон",
  "voice_mode.vad": "голосовая активация",
  "voice_mode.ptt": "нажми и говори",
  "voice.connected": "подключено",
  "voice.no_rx": "нет приёма — регистрация\u2026",
  "voice.leave_call": "выйти из звонка",
  "voice.toggle_mode": "режим: {mode} (клик: vad ↔ ptt)",

  "common.mute": "выключить микрофон",
  "common.unmute": "включить микрофон",
  "common.disconnect": "отключиться",
  "common.connect": "подключиться",
  "common.cancel": "отмена",
  "common.on": "вкл",
  "common.off": "выкл",
  "common.add": "добавить",
  "common.copy": "копировать",
  "common.close": "закрыть",
  "common.appearance": "оформление",
  "common.nickname": "имя",
  "common.identicon": "идентикон",
  "common.none": "(нет)",
  "common.test": "тест",

  "settings.title": "настройки",
  "settings.esc_close": "esc · закрыть",
  "settings.notifications_desc": "системные уведомления, когда окно не в фокусе",
  "settings.tray_desc": "сворачивать в трей вместо выхода",
  "settings.autostart_desc": "запускать при входе в систему",
  "settings.hotkey_format": "формат: например {example}, CommandOrControl+Shift+M, F19",
};

const dicts: Record<string, Dict> = { en, ru };

export type Lang = "en" | "ru";

export const SUPPORTED_LANGS: { code: Lang; label: string }[] = [
  { code: "en", label: "English" },
  { code: "ru", label: "Русский" },
];

/// Translate a key, optionally interpolating `{name}` placeholders.
/// Falls back to English, then to the key itself.
export function t(key: string, params?: Record<string, string | number>): string {
  const lang = (state.status?.language ?? "en") as Lang;
  const raw = dicts[lang]?.[key] ?? en[key] ?? key;
  if (!params) return raw;
  return raw.replace(/\{(\w+)\}/g, (_, name) =>
    params[name] !== undefined ? String(params[name]) : `{${name}}`,
  );
}
