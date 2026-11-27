# Phoenix Android Schema Reference (versionCode `356207`)

> Auto-generated from `base.apk` by `scripts/extract-schema.py`. Use this instead of re-disassembling `classes.dex`.

## 1. ViewKinds (29)

`swipeFill`, `canvas`, `view`, `text`, `stack`, `image`, `list`, `scroll`, `hscroll`, `flow`, `navigator`, `input`, `sheet`, `modal`, `banner`, `popover`, `systemOverlay`, `player`, `lottie`, `rive`, `video`, `webframe`, `zoom`, `ring`, `spinner`, `datePicker`, `markup`, `slider`, `vector_anim`

## 2. Native Overlay View Props

### `banner`
- `edge`: "top" | "bottom" (top auto-pads status bar + cutout)
- `passthrough`: bool (false = card intercepts taps, outside passes through)
- `announce`: string (accessibility announcement)
- `announcePolite`: bool

### `sheet`
- `dismissible`: bool
- `expanded`: bool
- `expandSeq`: i32
- `handleColor`: ARGB color
- `handleHidden`: bool
- `handleOverlay`: bool
- `sheetClip`: bool
- `sheetMaxHeight`: float
- `scrimColor`: ARGB color
- `scrimOpacity`: float
- `background`: ARGB color
- `radius / radiusTopLeft`: float
- `on`: ["dismiss"]

### `popover`
- `placement`: "top" | "bottom" | "left" | "right" | "auto"
- `align`: "start" | "center" | "end"
- `edgeInset`: float
- `offset`: float
- `passthrough`: bool
- `underSheets`: bool

### `modal`
- `dismissible`: bool
- `scrim`: bool
- `scrimColor`: ARGB color
- `scrimOpacity`: float
- `backdropBlur`: float

## 3. Wire Type Cheat-Sheet (Avoid Kotlin `ClassCastException`)

- **float (Value::F64)**: `opacity`, `scrimOpacity`, `width`, `height`, `minWidth`, `maxWidth`, `minHeight`, `maxHeight`, `radius`, `radiusTopLeft`, `radiusTopRight`, `radiusBottomLeft`, `radiusBottomRight`, `fontSize`, `lineHeight`, `gap`, `gapX`, `gapY`, `flex`, `scale`, `scaleX`, `scaleY`, `translateX`, `translateY`, `rotation`, `elevation`, `borderWidth`, `hitSlop`
- **rect (Value::Rect([l,t,r,b]))**: `padding`, `margin`, `activePadding`, `activeMargin`, `rangeInsets`, `scrollIndicatorInsets`
- **color (Value::I64/I32 ARGB)**: `color`, `background`, `activeBackground`, `borderColor`, `tint`, `scrimColor`, `navScrimColor`, `handleColor`, `placeholderColor`, `focusBorderColor`, `minTrackTint`, `maxTrackTint`, `thumbTint`, `elevationColor`, `bg`, `bgBorder`
- **int (Value::I32)**: `fontWeight`, `maxLines`, `index`, `first`, `seq`, `expandSeq`
- **bool (Value::Bool)**: `hidden`, `clip`, `selectable`, `multiline`, `secure`, `disabled`, `passthrough`, `announcePolite`, `dismissible`, `expanded`, `handleHidden`, `underSheets`, `lineHeightExact`, `includeFontPadding`

## 4. Host Modules & Methods (`hh.jb` — 79 modules, 433 methods)

Call via `phoenix.bridge.invoke(moduleId, methodId, args, body)`.

| Module | ID | Methods (`name: id`) |
| --- | ---: | --- |
| `Access` | `1` | `signIn: 64`, `signOut: 65`, `status: 66` |
| `AdvertisingId` | `2` | `fetch: 67` |
| `AgeSignals` | `3` | `get: 68`, `info: 69` |
| `Announcer` | `4` | `announce: 70` |
| `AppAttest` | `5` | `isSupported: 71`, `generateAndAttestKey: 451`, `generateAssertion: 452` |
| `AppIcon` | `6` | `current: 72`, `set: 73` |
| `AppReview` | `7` | `request: 74` |
| `AppShortcuts` | `8` | `pin: 75`, `setAccounts: 76`, `setChannels: 77`, `launchAccount: 2394` |
| `AppState` | `9` | `exit: 78`, `hide: 79`, `restart: 80`, `setBackHandler: 81`, `setHomeIndicator: 82`, `setReduceMotion: 83`, `setTheme: 84` |
| `BackgroundSync` | `10` | `cancel: 85`, `finish: 86`, `schedule: 87` |
| `BackgroundTask` | `11` | `begin: 88`, `end: 89`, `uploads: 1451` |
| `BackgroundUploads` | `12` | `finish: 90`, `forget: 91`, `list: 92`, `start: 93` |
| `Browser` | `13` | `clearWebsiteData: 94`, `info: 95`, `open: 96`, `openStoreInline: 97`, `closeAuthSession: 2160`, `openAuthSession: 2252`, `openTracked: 2410`, `resumeTracked: 2727` |
| `BugReporter` | `14` | `watch: 98`, `watchShake: 464` |
| `BundledMusic` | `15` | `pause: 99`, `play: 100`, `stop: 101` |
| `ClientSwitch` | `16` | `get: 102`, `set: 103` |
| `Clipboard` | `17` | `getString: 104`, `setItem: 105`, `setString: 106` |
| `Contacts` | `18` | `permission: 107`, `read: 108`, `status: 1798` |
| `CrashReports` | `19` | `keepLogs: 109`, `noteFeature: 110`, `setConsent: 111`, `reportStall: 1686`, `clearUser: 3026`, `setUser: 3229` |
| `Crypto` | `20` | `decrypt: 453`, `dropKey: 454`, `generateKey: 455` |
| `Data` | `21` | `read: 112`, `installLanguage: 462` |
| `Database` | `22` | `open: 379`, `get: 380`, `scan: 381`, `write: 382`, `clear: 383`, `usage: 384`, `compact: 385`, `optimize: 386`, `close: 387`, `destroy: 388`, `list: 389`, `getMany: 2838`, `openReadOnly: 2950` |
| `DebugLogs` | `23` | `read: 113` |
| `DeviceInfo` | `24` | `constants: 114`, `setTextZoom: 1699` |
| `DeviceSilentMode` | `25` | `start: 115`, `stop: 116` |
| `Diagnostics` | `26` | `clearImageCache: 117`, `resetFrames: 118`, `resetImageBudgets: 119`, `setFpsMonitor: 120`, `setFrameTiming: 121`, `setImageBudget: 122`, `snapshot: 123` |
| `DiscordVariants` | `27` | `current: 124`, `installed: 125`, `launch: 126` |
| `FoldableLayout` | `28` | `seam: 127` |
| `FrameTrace` | `29` | `commit: 128` |
| `GooglePlayBilling` | `30` | `country: 129`, `loadPurchases: 130`, `open: 131`, `products: 132`, `purchase: 133`, `setFake: 134`, `close: 456` |
| `Haptics` | `31` | `pattern: 135`, `perform: 136` |
| `ImageLoads` | `32` | `retry: 137`, `schedule: 138`, `fetch: 415`, `decode: 416`, `load: 467`, `cancel: 468`, `evict: 1435`, `pin: 1876`, `pinStats: 3337` |
| `Images` | `33` | `crop: 139`, `markup: 140`, `pixels: 141`, `videoSize: 1866`, `size: 2365` |
| `IosAttribution` | `34` | `endImpression: 142`, `info: 143`, `startImpression: 144`, `updateConversionValue: 145` |
| `KeyCommands` | `35` | `set: 146` |
| `Keyboard` | `36` | `dismiss: 147` |
| `Linking` | `37` | `openAccessibilitySettings: 148`, `openSettings: 149`, `openURL: 150`, `openNotificationSettings: 2141` |
| `LiveActivity` | `38` | `downloadImages: 151`, `end: 152`, `update: 153` |
| `Lottie` | `39` | — |
| `Media` | `40` | `save: 154` |
| `MediaPicker` | `41` | `capture: 155`, `library: 156`, `manageLimited: 157`, `pick: 158`, `read: 159`, `release: 160`, `watch: 161`, `describe: 470`, `cancelExport: 1573`, `export: 3377` |
| `MediaTranscoder` | `42` | `cancel: 162`, `capabilities: 163`, `removeTempFile: 164`, `tempFiles: 165`, `videoEncodingSupport: 166`, `decodeImage: 457`, `encodeImage: 458`, `probeVideo: 459`, `transcodeVideo: 460`, `rewriteOriginal: 3099`, `mediaOrigin: 3281`, `imageQuality: 3415` |
| `Networking` | `43` | `cancel: 167`, `request: 168`, `upload: 169` |
| `Orientation` | `44` | `info: 170`, `lock: 171` |
| `Passkeys` | `45` | `cancel: 172`, `create: 173`, `get: 174` |
| `Perf` | `46` | `collectStorageDiagnostics: 175`, `drainFrames: 176`, `lastExit: 177`, `mark: 178`, `marks: 179`, `measureCaches: 180`, `processStats: 181`, `ackCrashReports: 1762`, `launchSettled: 1979`, `crashReports: 2175` |
| `Push` | `47` | `accounts: 182`, `appendReply: 183`, `badge: 184`, `cancel: 185`, `cancelLocal: 186`, `channelImportances: 187`, `channelStates: 188`, `channels: 189`, `dismiss: 190`, `finish: 191`, `incomingRingtone: 192`, `openChannelSettings: 193`, `permission: 194`, `requestProvisional: 195`, `requestToken: 196`, `reset: 197`, `schedule: 198`, `scheduledLocal: 199`, `show: 200`, `legacyBehaviors: 1419`, `withdraw: 1437`, `headlessReady: 1988`, `clearChannels: 2794` |
| `Scanner` | `48` | `scanQr: 201` |
| `ScreenWakeLock` | `49` | `request: 202`, `release: 203` |
| `Share` | `50` | `apps: 204`, `cancel: 205`, `mail: 206`, `media: 207`, `setTargets: 208`, `sms: 209`, `text: 210` |
| `SmsConsent` | `51` | `start: 211`, `stop: 212` |
| `Storage` | `52` | `clearProduction: 213`, `get: 214`, `production: 215`, `remove: 216`, `set: 217` |
| `StoreKit` | `53` | `canMakePayments: 218`, `clearTransactions: 219`, `finish: 220`, `manageSubscriptions: 221`, `pending: 222`, `products: 223`, `purchase: 224`, `purchases: 225`, `restore: 226`, `storefront: 227`, `watchTransactions: 3317` |
| `SystemOverlay` | `54` | `openApp: 228`, `request: 229`, `status: 230`, `toast: 231` |
| `TelemetryRing` | `55` | `setTouchLogging: 232` |
| `TextMetrics` | `56` | `emojiNickNudge: 233`, `textWidth: 466` |
| `Timing` | `57` | `clear: 234`, `set: 235` |
| `Tracking` | `58` | `request: 236`, `status: 237` |
| `UIManager` | `59` | `batchEvents: 238`, `warmUp: 1428` |
| `Updates` | `60` | `autoUpdate: 239`, `branches: 240`, `developerMode: 241`, `restart: 242`, `setBranch: 243`, `status: 244`, `check: 461`, `updateMode: 1469`, `testers: 3257` |
| `Voice` | `61` | `broadcastRequestResult: 245`, `capabilities: 246`, `clearNowPlaying: 247`, `close: 248`, `connect: 249`, `engine: 250`, `headlessVoice: 251`, `messageCapabilities: 252`, `muteState: 253`, `op: 254`, `permissionState: 255`, `playBundledSound: 256`, `playPause: 257`, `playSeek: 258`, `playSound: 259`, `playStart: 260`, `playStop: 261`, `prewarmNetwork: 262`, `recordStart: 263`, `recordStop: 264`, `releaseFile: 265`, `requestPermission: 266`, `requestScreenShare: 267`, `ring: 268`, `ringEnded: 269`, `session: 270`, `setAppState: 271`, `setAudioRoute: 272`, `setKeepAlive: 273`, `setPip: 274`, `setPipSource: 275`, `setSystemMute: 276`, `showAudioRoutePicker: 277`, `showMicrophoneModes: 278`, `stopScreenShare: 279`, `strings: 280`, `unmuteSystemInput: 281`, `watchInputLevel: 282`, `setSystemInputMuted: 1472`, `setCallSilence: 1493`, `ringDeclined: 2108`, `backgroundRestricted: 2196`, `rankRtcRegions: 2920`, `stopSound: 2949`, `stopAllSounds: 3090`, `detachRefused: 3310` |
| `WebFrame` | `62` | `close: 283`, `decide: 284`, `deliver: 285`, `frameScript: 286`, `info: 287`, `open: 288` |
| `WebSocket` | `63` | `close: 289`, `dropEarly: 290`, `open: 291`, `sendBinary: 292`, `sendText: 293`, `adopt: 440` |
| `HostRules` | `1183` | `classify: 1184`, `post: 1185`, `readClears: 1186`, `channelLink: 1187`, `ringTagPrefix: 1188`, `ringWindow: 1189`, `offeredRoutes: 1190`, `autoRoute: 1191`, `sharedFileCap: 1192`, `retryWait: 1193`, `imageBudgets: 1194`, `cachePolicy: 1195`, `imageVariants: 1196`, `shareHandoffUrl: 1197`, `shareHandoffRequest: 1198`, `shareImage: 1199`, `shareVideo: 1200`, `shareVideoStaged: 1201`, `shareImageChoice: 1202`, `stackLaunch: 1453`, `stackRefuses: 1454`, `backgroundPush: 1479`, `shareExtension: 1581`, `decodeThreads: 1586`, `signInRequired: 1634`, `lottieEdits: 1687`, `pickedThumb: 1862`, `rtcRegionPing: 1893`, `frameTraceLine: 1916`, `smoothSeekShown: 1954`, `voiceMonitorSent: 1960`, `riveAllowed: 2244`, `readClearsAndroid: 2411`, `smoothSeek: 2413`, `inboxActive: 2527`, `writeRebase: 2545`, `pickedFrame: 2640`, `riveIsRive: 2660`, `ackClears: 2766`, `frameTraceInputs: 2807`, `voiceMonitorStep: 2877`, `pickedExport: 2965`, `voiceMonitorArgs: 2979`, `riveLimits: 3024`, `inboxSave: 3060`, `callInterruption: 3170`, `signInBackoff: 3185`, `imageCrop: 3223`, `firstFrameClock: 3269` |
| `AccessSignIn` | `1255` | `metadata: 1256`, `login: 1257`, `poll: 1258`, `cancel: 1259`, `fresh: 1260`, `needsSignIn: 1261`, `sameOrigin: 1262`, `exchangeStart: 1263`, `exchange: 1264` |
| `CrashReporter` | `1281` | `launch: 1282`, `report: 1283`, `reportTrap: 1284`, `setNonFatalTarget: 1285`, `noteFeature: 1286`, `outbox: 1287`, `uploaded: 1288`, `tombstoneSummary: 1289`, `metrickitFrames: 1290`, `crashForTest: 1291`, `setUser: 1681`, `reportStall: 3202`, `firstFrameDrawn: 3343`, `firstFrameMissed: 3517` |
| `Ota` | `1323` | `launch: 1324`, `uiLaunch: 1325`, `abandonUiLaunch: 1326`, `confirm: 1327`, `fail: 1328`, `reject: 1329`, `manifestUrl: 1330`, `signed: 1331`, `offerFiles: 1332`, `stage: 1333`, `decodeModule: 1334`, `applyDelta: 1335`, `compiledPath: 1336`, `compile: 1337`, `events: 1338`, `report: 1339`, `status: 1340`, `setBranch: 1341`, `trackerUrl: 1342`, `branches: 1343`, `trackerBuild: 1344`, `setUpdateMode: 1466`, `holdCompile: 1741`, `testersBase: 1935`, `takeExit: 2131`, `keepManifestEtag: 2171`, `noteExit: 2723`, `manifestEtag: 3298` |
| `PushContent` | `1395` | `content: 1396`, `reply: 1397`, `receive: 1576`, `withdrawnShown: 1861`, `messageFetched: 2481`, `messageFetch: 3052`, `withdrawnRecord: 3147`, `present: 3342` |
| `RnStrings` | `1403` | `load: 1404`, `assets: 1405`, `setLocale: 1406`, `compose: 1407`, `plan: 1864`, `packLocale: 2183` |
| `PlayIntegrity` | `1439` | `prepare: 1440`, `request: 1441` |
| `DigitalCredentials` | `1445` | `isAvailable: 1446`, `get: 1447` |
| `OnDemandResource` | `1473` | `has: 1474`, `fetch: 1475` |
| `ProductionWrite` | `1738` | `write: 3334` |
| `AppTransaction` | `2010` | `fetch: 2435` |
| `PlaybackSession` | `2402` | `setEnabledForNextLaunch: 3005` |
| `DatePickerDialog` | `2465` | `close: 2320`, `open: 2470` |
| `CompanionAuth` | `2745` | `sendAuthUrl: 3157` |
| `BackgroundSpans` | `3222` | `recent: 1972` |
| `UserActivity` | `3459` | `resign: 2165`, `set: 3401` |

## 5. Host Dispatch Events (`@event` — 109)

`@access: 294`, `@accessibilityService: 295`, `@accountShortcut: 296`, `@advertisingId: 297`, `@ageSignals: 298`, `@app: 299`, `@appAttest: 300`, `@appIcon: 301`, `@appInactive: 302`, `@appearance: 303`, `@appsflyer: 304`, `@artProfile: 305`, `@backgroundSync: 306`, `@backgroundUploads: 307`, `@batch: 308`, `@browser: 309`, `@cacheSize: 310`, `@channelStates: 311`, `@connected: 312`, `@contacts: 313`, `@cropped: 314`, `@crypto: 315`, `@foldSeam: 316`, `@fontScale: 317`, `@frame: 318`, `@frameTimes: 319`, `@googlePlayBilling: 320`, `@http: 321`, `@imageLoad: 322`, `@imageWanted: 323`, `@iosAttribution: 324`, `@library: 325`, `@libraryChanged: 326`, `@link: 327`, `@marked: 328`, `@memory: 329`, `@metricKitPayload: 330`, `@network: 331`, `@passkey: 332`, `@permission: 333`, `@picked: 334`, `@pixels: 335`, `@playFinished: 336`, `@push: 337`, `@read: 338`, `@recordPermission: 339`, `@recorded: 340`, `@reduceMotion: 341`, `@resolution: 342`, `@saved: 343`, `@scanner: 344`, `@scene: 345`, `@screenReader: 346`, `@screenshot: 347`, `@share: 348`, `@shareAppClicked: 349`, `@shareStaged: 350`, `@silentMode: 351`, `@smsConsent: 352`, `@stall: 353`, `@storageDiagnostics: 354`, `@storekit: 355`, `@storekitCanMakePayments: 356`, `@storekitCleared: 357`, `@storekitManaged: 358`, `@storekitPending: 359`, `@storekitProducts: 360`, `@storekitPurchase: 361`, `@storekitPurchases: 362`, `@storekitStorefront: 363`, `@systemOverlay: 364`, `@telemetry: 365`, `@thermalState: 366`, `@timeChanged: 367`, `@timer: 368`, `@token: 369`, `@tracking: 370`, `@transcode: 371`, `@transcodeProgress: 372`, `@turnStats: 373`, `@updates: 374`, `@upload: 375`, `@voice: 376`, `@webframe: 377`, `@ws: 378`, `@languageInstall: 463`, `@shake: 465`, `@described: 471`, `@musicLoad: 1423`, `@keyboardSettled: 1424`, `@hardwareKeyboard: 1425`, `@playIntegrity: 1442`, `@digitalCredential: 1448`, `@onDemandResource: 1476`, `@authSessionFinished: 1552`, `@exported: 1631`, `@appTransaction: 1633`, `@pinsMeasured: 1822`, `@imageSize: 1926`, `@smsResult: 2204`, `@modalNavBar: 2232`, `@prefersCrossFade: 2506`, `@rtcRegionLatency: 2507`, `@exportProgress: 2872`, `@announcementFinished: 3114`, `@storekitTransactionUpdated: 3231`, `@navBar: 3272`, `@datePickerDialog: 3520`, `@companionAuth: 3576`

## 6. Known `accessibilityId` Patterns (28)

`Record Voice Message`, `composer.expressions`, `composer.gift`, `expression.emoji:custom:`, `message.actions.`, `message.actions.copy_id`, `message.actions.copy_link`, `message.actions.copy_text`, `message.actions.delete`, `message.actions.edit`, `message.actions.mark_unread`, `message.actions.mention`, `message.actions.reply`, `message.actions.report`, `message.actions.root`, `message.author:`, `message.content:`, `message.timestamp:`, `phoenix.command.{name}`, `phoenix.in-app-notification`, `phoenix.in-app-notification.body`, `phoenix.in-app-notification.dismiss`, `phoenix.{}.{}`, `profile-tab-pages`, `search.entry`, `send`, `settings.row.logout`, `suggestions`

## 7. Mana Icons (`res://ic_mana_*` — 286 total)

Full list is in `phoenix-356207.json` under `manaIcons`. Commonly used icons:

`res://ic_mana_angle_brackets`, `res://ic_mana_chat_eye`, `res://ic_mana_emoji_face_with_crossed_out_eyes`, `res://ic_mana_emoji_face_with_raised_eyebrow`, `res://ic_mana_emoji_grinning_face_with_big_eyes`, `res://ic_mana_emoji_grinning_face_with_smiling_eyes`, `res://ic_mana_emoji_kissing_face_with_closed_eyes`, `res://ic_mana_emoji_sad_face_with_crossed_out_eyes`, `res://ic_mana_emoji_smiling_face_with_smiling_eyes`, `res://ic_mana_experimental_microphone_sparkle`, `res://ic_mana_experimental_microphone_sparkle_muted`, `res://ic_mana_eye_plus`, `res://ic_mana_image_file_up`, `res://ic_mana_puzzle_piece_minus`, `res://ic_mana_puzzle_piece_plus`, `res://ic_mana_puzzle_piece_retry`, `res://ic_mana_shop_sparkle`, `res://ic_mana_x_small`
