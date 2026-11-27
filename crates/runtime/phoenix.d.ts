// Types for the `phoenix` object every plugin bundle receives.

declare namespace Phoenix {
  /// Partial user structure, resources/user. `public_flags` is User Flags.
  interface User {
    id: string;
    username: string;
    discriminator: string;
    global_name?: string | null;
    avatar: string | null;
    avatar_decoration_data?: AvatarDecorationData | null;
    collectibles?: Collectibles | null;
    display_name_styles?: DisplayNameStyle | null;
    primary_guild?: PrimaryGuild | null;
    bot?: boolean;
    system?: boolean;
    banner?: string | null;
    accent_color?: number | null;
    public_flags?: number;
  }

  /// The signed-in user as Phoenix cached it, resources/user#user-object with the private fields.
  interface CurrentUser extends User {
    email?: string | null;
    verified?: boolean;
    mfa_enabled?: boolean;
    premium_type?: number;
    flags?: number;
    locale?: string;
    nsfw_allowed?: boolean | null;
    age_verification_status?: number;
  }

  /// `expires_at` is a Unix timestamp, not ISO.
  interface AvatarDecorationData {
    asset: string;
    sku_id: string;
    expires_at: number | null;
  }

  interface Collectibles {
    nameplate: NameplateData | null;
  }

  interface NameplateData {
    asset: string;
    sku_id: string;
    label: string;
    palette: string;
    expires_at: number | null;
  }

  /// `font_id` is Display Name Font, `effect_id` is Display Name Effect.
  interface DisplayNameStyle {
    font_id: number;
    effect_id: number;
    colors: number[];
  }

  interface PrimaryGuild {
    identity_enabled: boolean | null;
    identity_guild_id: string | null;
    tag: string | null;
    badge: string | null;
  }

  /// `flags` is Guild Member Flags.
  interface GuildMember {
    user: User;
    nick?: string | null;
    avatar?: string | null;
    avatar_decoration_data?: AvatarDecorationData | null;
    collectibles?: Collectibles | null;
    display_name_styles?: DisplayNameStyle | null;
    banner?: string | null;
    bio?: string | null;
    roles: string[];
    joined_at: string;
    premium_since?: string | null;
    deaf?: boolean;
    mute?: boolean;
    pending?: boolean;
    communication_disabled_until?: string | null;
    unusual_dm_activity_until?: string | null;
    flags: number;
    permissions?: string;
  }

  type PartialGuildMember = Omit<GuildMember, "user">;

  interface MentionedUser extends User {
    member?: PartialGuildMember;
  }

  /// `name` is null for deleted custom emoji, though the docs type it as string.
  interface Emoji {
    id: string | null;
    name: string | null;
    roles?: string[];
    user?: User;
    require_colons?: boolean;
    managed?: boolean;
    animated?: boolean;
    available?: boolean;
    version?: string;
  }

  type PartialEmoji = Pick<Emoji, "id" | "name"> & Partial<Emoji>;

  /// `color` is deprecated in favor of `colors`.
  interface Role {
    id: string;
    name: string;
    description: string | null;
    color: number;
    colors: RoleColors;
    hoist: boolean;
    icon?: string | null;
    unicode_emoji?: string | null;
    position: number;
    permissions: string;
    managed: boolean;
    mentionable: boolean;
    flags?: number;
    tags?: RoleTags;
    version?: string;
  }

  interface RoleColors {
    primary_color: number;
    secondary_color: number | null;
    tertiary_color: number | null;
  }

  /// The `null` fields are flags, present when true and absent when false.
  interface RoleTags {
    bot_id?: string;
    integration_id?: string;
    premium_subscriber?: null;
    subscription_listing_id?: string;
    available_for_purchase?: null;
    guild_connections?: null;
  }

  /// `type` is Sticker Types, `format_type` is Sticker Format Types.
  interface Sticker {
    id: string;
    pack_id?: string;
    name: string;
    description: string | null;
    tags: string;
    type: number;
    format_type: number;
    available?: boolean;
    guild_id?: string;
    user?: User;
    sort_value?: number;
    version?: string;
  }

  interface StickerItem {
    id: string;
    name: string;
    format_type: number;
  }

  interface SoundboardSound {
    sound_id: string;
    name: string;
    volume: number;
    emoji_id: string | null;
    emoji_name: string | null;
    guild_id?: string;
    available: boolean;
    user?: User;
    user_id?: string;
  }

  /// `flags` is Attachment Flags.
  interface Attachment {
    id: string;
    filename: string;
    title?: string;
    description?: string;
    content_type?: string;
    size: number;
    url: string;
    proxy_url: string;
    height?: number | null;
    width?: number | null;
    content_scan_version?: number;
    placeholder_version?: number;
    placeholder?: string;
    ephemeral?: boolean;
    duration_secs?: number;
    waveform?: string;
    flags?: number;
    is_clip?: boolean;
    is_thumbnail?: boolean;
    is_remix?: boolean;
    is_spoiler?: boolean;
    clip_created_at?: string;
    clip_participants?: User[];
    application?: PartialApplication | null;
  }

  /// Narrowed from the partial application structure on resources/application.
  interface PartialApplication {
    id: string;
    name: string;
    description: string;
    icon: string | null;
    cover_image?: string;
    splash?: string;
    type: number | null;
    flags: number;
    [key: string]: unknown;
  }

  /// `type` is Embed Type, `flags` is Embed Flags.
  interface Embed {
    title?: string;
    type?: string;
    description?: string;
    url?: string;
    timestamp?: string;
    color?: number;
    footer?: EmbedFooter;
    image?: EmbedMedia;
    thumbnail?: EmbedMedia;
    video?: EmbedMedia;
    provider?: EmbedProvider;
    author?: EmbedAuthor;
    fields?: EmbedField[];
    reference_id?: string;
    content_scan_version?: number;
    flags?: number;
  }

  /// `flags` is Attachment Flags.
  interface EmbedMedia {
    url: string;
    proxy_url?: string;
    height?: number;
    width?: number;
    flags?: number;
    description?: string;
    content_type?: string;
    content_scan_metadata?: ContentScanMetadata;
    placeholder_version?: number;
    placeholder?: string;
  }

  interface EmbedProvider {
    name?: string;
    url?: string;
  }

  interface EmbedAuthor {
    name: string;
    url?: string;
    icon_url?: string;
    proxy_icon_url?: string;
  }

  interface EmbedFooter {
    text: string;
    icon_url?: string;
    proxy_icon_url?: string;
  }

  interface EmbedField {
    name: string;
    value: string;
    inline?: boolean;
  }

  /// `flags` is Content Scan Flags.
  interface ContentScanMetadata {
    flags: number;
    version: number;
  }

  interface Reaction {
    count: number;
    count_details: ReactionCountDetails;
    me: boolean;
    me_burst: boolean;
    emoji: PartialEmoji;
    burst_colors: string[];
  }

  interface ReactionCountDetails {
    normal: number;
    burst: number;
  }

  /// `type` is Activity Action Type on resources/presence.
  interface MessageActivity {
    type: number;
    party_id?: string;
    name_override?: string;
    icon_override?: string;
  }

  interface MessageCall {
    participants: string[];
    ended_timestamp: string | null;
  }

  /// `type` is Interaction Type, `command_type` is Application Command Type.
  interface MessageInteractionMetadata {
    id: string;
    type: number;
    name?: string;
    command_type?: number;
    ephemerality_reason?: number;
    user: User;
    authorizing_integration_owners: Record<string, string>;
    original_response_message_id?: string;
    interacted_message_id?: string;
    triggering_interaction_metadata?: MessageInteractionMetadata;
    target_user?: User;
    target_message_id?: string;
  }

  interface MessageInteraction {
    id: string;
    type: number;
    name: string;
    user: User;
  }

  interface MessageRoleSubscription {
    role_subscription_listing_id: string;
    tier_name: string;
    total_months_subscribed: number;
    is_renewal: boolean;
  }

  /// `type` is Message Purchase Notification Type.
  interface MessagePurchaseNotification {
    type: number;
    guild_product_purchase: GuildProductPurchase | null;
  }

  interface GuildProductPurchase {
    listing_id: string;
    product_name: string;
  }

  interface MessageGiftInfo {
    emoji?: PartialEmoji;
    sound?: { id: string };
  }

  /// `type` is Message Reference Type, 0 for replies and 1 for forwards.
  interface MessageReference {
    type?: number;
    message_id?: string;
    channel_id: string;
    guild_id?: string;
  }

  interface MessageSnapshot {
    message: SnapshotMessage;
  }

  interface SnapshotMessage {
    content: string;
    timestamp: string;
    edited_timestamp: string | null;
    mentions: User[];
    mention_roles: string[];
    attachments: Attachment[];
    embeds: Embed[];
    type: number;
    flags: number;
    components?: Component[];
    resolved?: ResolvedData;
    sticker_items?: StickerItem[];
    soundboard_sounds?: SoundboardSound[];
  }

  /// `layout_type` is Poll Layout Type.
  interface Poll {
    question: PollMedia;
    answers: PollAnswer[];
    expiry: string | null;
    allow_multiselect: boolean;
    layout_type: number;
    results?: PollResults;
  }

  interface PollMedia {
    text?: string;
    emoji?: PartialEmoji;
  }

  interface PollAnswer {
    answer_id: number;
    poll_media: PollMedia;
  }

  interface PollResults {
    is_finalized: boolean;
    answer_counts: PollAnswerCount[];
  }

  interface PollAnswerCount {
    id: number;
    count: number;
    me_voted: boolean;
  }

  /// `type` is Potion Type.
  interface Potion {
    used_by: string;
    type: number;
    emoji: PartialEmoji[];
    created_at: string;
  }

  /// `base_theme` is Theme on resources/user-settings-proto.
  interface SharedClientTheme {
    colors: string[];
    gradient_angle: number;
    base_mix: number;
    base_theme?: number | null;
  }

  /// `type` is Application Type on resources/application.
  interface IntegrationApplication {
    id: string;
    name: string;
    description: string;
    icon: string | null;
    cover_image?: string;
    splash?: string;
    type: number | null;
    primary_sku_id?: string;
    bot?: User;
    deeplink_uri?: string | null;
    third_party_skus?: ApplicationSku[];
    role_connections_verification_url?: string | null;
    connection_entrypoint_url?: string;
    is_verified: boolean;
    is_discoverable: boolean;
    is_monetized: boolean;
    parent_id?: string;
  }

  /// Application SKU object, resources/game.
  interface ApplicationSku {
    id: string | null;
    sku: string | null;
    distributor: string;
  }

  /// `type` is Channel Type, `flags` is Channel Flags.
  interface Channel {
    id: string;
    type: number;
    guild_id?: string;
    position?: number;
    permission_overwrites?: PermissionOverwrite[];
    name?: string | null;
    topic?: string | null;
    nsfw?: boolean;
    last_message_id?: string | null;
    last_pin_timestamp?: string | null;
    bitrate?: number;
    user_limit?: number;
    rate_limit_per_user?: number;
    recipients?: User[];
    recipient_flags?: number;
    icon?: string | null;
    nicks?: ChannelNick[];
    managed?: boolean;
    blocked_user_warning_dismissed?: boolean;
    safety_warnings?: SafetyWarning[];
    application_id?: string;
    owner_id?: string;
    owner?: GuildMember | null;
    parent_id?: string | null;
    rtc_region?: string | null;
    video_quality_mode?: number;
    total_message_sent?: number;
    message_count?: number;
    member_count?: number;
    member_ids_preview?: string[];
    thread_metadata?: ThreadMetadata;
    member?: ThreadMember;
    default_auto_archive_duration?: number | null;
    default_thread_rate_limit_per_user?: number;
    permissions?: string;
    flags?: number;
    available_tags?: ForumTag[];
    applied_tags?: string[];
    default_reaction_emoji?: DefaultReaction | null;
    default_forum_layout?: number;
    default_sort_order?: number | null;
    default_tag_setting?: string;
    icon_emoji?: IconEmoji | null;
    theme_color?: number | null;
    is_message_request?: boolean;
    is_message_request_timestamp?: string | null;
    is_spam?: boolean;
    status?: string | null;
    hd_streaming_until?: string | null;
    hd_streaming_buyer_id?: string | null;
    linked_lobby?: LinkedLobby | null;
    is_linkable?: boolean;
    is_viewable_and_writeable_by_all_members?: boolean;
    template?: string;
    version?: string;
  }

  interface PartialChannel {
    id: string;
    type: number;
    name: string | null;
    recipients?: User[];
    icon?: string | null;
    guild_id?: string | null;
  }

  /// `type` is Permission Overwrite Type, 0 for role and 1 for member.
  interface PermissionOverwrite {
    id: string;
    type: number;
    allow: string;
    deny: string;
  }

  interface ChannelNick {
    id: string;
    nick: string;
  }

  /// `type` is Safety Warning Type.
  interface SafetyWarning {
    id: string;
    type: number;
    expiry: string;
    dismiss_timestamp: string | null;
  }

  interface ThreadMetadata {
    archived: boolean;
    auto_archive_duration: number;
    archive_timestamp: string;
    locked: boolean;
    invitable?: boolean;
    create_timestamp?: string | null;
  }

  /// `flags` is Thread Member Flags.
  interface ThreadMember {
    id?: string;
    user_id?: string;
    join_timestamp: string;
    flags: number;
    muted?: boolean;
    mute_config?: MuteConfig | null;
    member?: GuildMember;
  }

  interface MuteConfig {
    end_time?: string | null;
    selected_time_window?: number;
  }

  interface ForumTag {
    id: string;
    name: string;
    moderated: boolean;
    emoji_id: string | null;
    emoji_name: string | null;
  }

  interface DefaultReaction {
    emoji_id: string | null;
    emoji_name: string | null;
  }

  interface IconEmoji {
    id: string | null;
    name: string | null;
  }

  interface LinkedLobby {
    application_id: string;
    lobby_id: string;
    linked_by: string;
    linked_at: string;
    require_application_authorization: boolean;
  }

  /// Resolved data object, interactions/receiving-and-responding. Maps are keyed by snowflake.
  interface ResolvedData {
    users?: Record<string, User>;
    members?: Record<string, Omit<GuildMember, "user" | "deaf" | "mute">>;
    roles?: Record<string, Role>;
    channels?: Record<
      string,
      Omit<Channel, "permission_overwrites" | "recipients" | "icon" | "application_id" | "managed" | "member">
    >;
    messages?: Record<string, Message>;
    attachments?: Record<string, Attachment>;
  }

  /// Message components from resources/components. Modal-only types are left out.
  type Component =
    | ActionRow
    | Button
    | StringSelect
    | UserSelect
    | RoleSelect
    | MentionableSelect
    | ChannelSelect
    | Section
    | TextDisplay
    | Thumbnail
    | MediaGallery
    | FileComponent
    | Separator
    | ContentInventoryEntryComponent
    | Container
    | CheckpointCard;

  interface ActionRow {
    type: 1;
    id?: number;
    components: Component[];
  }

  /// `style` is Button Style. The docs mark `custom_id` required, but link and premium buttons omit it.
  interface Button {
    type: 2;
    id?: number;
    style: number;
    label?: string;
    emoji?: PartialEmoji;
    custom_id?: string;
    sku_id?: string;
    url?: string;
    disabled?: boolean;
  }

  interface SelectBase {
    id?: number;
    custom_id: string;
    placeholder?: string;
    min_values?: number;
    max_values?: number;
    required?: boolean;
    disabled?: boolean;
  }

  interface StringSelect extends SelectBase {
    type: 3;
    options: SelectOption[];
  }

  interface SelectOption {
    label: string;
    value: string;
    description?: string;
    emoji?: PartialEmoji;
    default?: boolean;
  }

  interface UserSelect extends SelectBase {
    type: 5;
    default_values?: SelectDefaultValue[];
  }

  interface RoleSelect extends SelectBase {
    type: 6;
    default_values?: SelectDefaultValue[];
  }

  interface MentionableSelect extends SelectBase {
    type: 7;
    default_values?: SelectDefaultValue[];
  }

  /// `channel_types` is Channel Type.
  interface ChannelSelect extends SelectBase {
    type: 8;
    channel_types?: number[];
    default_values?: SelectDefaultValue[];
  }

  interface SelectDefaultValue {
    id: string;
    type: string;
  }

  interface Section {
    type: 9;
    id?: number;
    components: TextDisplay[];
    accessory: Thumbnail | Button;
  }

  interface TextDisplay {
    type: 10;
    id?: number;
    content: string;
  }

  interface Thumbnail {
    type: 11;
    id?: number;
    media: UnfurledMediaItem;
    description?: string | null;
    spoiler?: boolean;
  }

  interface MediaGallery {
    type: 12;
    id?: number;
    items: MediaGalleryItem[];
  }

  interface MediaGalleryItem {
    media: UnfurledMediaItem;
    description?: string | null;
    spoiler?: boolean;
  }

  interface FileComponent {
    type: 13;
    id?: number;
    file: UnfurledMediaItem;
    spoiler?: boolean;
    name: string;
    size: number;
  }

  /// `spacing` is Separator Spacing Type.
  interface Separator {
    type: 14;
    id?: number;
    divider?: boolean;
    spacing?: number;
  }

  /// The entry is left untyped, see Content Inventory Entry Object on resources/components.
  interface ContentInventoryEntryComponent {
    type: 16;
    id?: number;
    content_inventory_entry: Record<string, unknown>;
  }

  interface Container {
    type: 17;
    id?: number;
    components: Component[];
    accent_color?: number | null;
    spoiler?: boolean;
  }

  /// The data is left untyped, see Checkpoint Data Structure on resources/components.
  interface CheckpointCard {
    type: 20;
    id?: number;
    checkpoint_data: Record<string, unknown>;
  }

  /// `flags` is Attachment Flags, `loading_state` is Unfurled Media Item Loading State.
  interface UnfurledMediaItem {
    id?: string;
    url: string;
    proxy_url?: string;
    height?: number | null;
    width?: number | null;
    flags?: number;
    content_type?: string;
    content_scan_metadata?: ContentScanMetadata;
    placeholder_version?: number;
    placeholder?: string;
    loading_state?: number;
    attachment_id?: string;
  }

  /// Message object, resources/message. `type` is Message Type, `flags` is Message Flags.
  interface Message {
    id: string;
    channel_id: string;
    lobby_id?: string;
    author: User;
    content: string;
    timestamp: string;
    edited_timestamp: string | null;
    tts: boolean;
    mention_everyone: boolean;
    mentions: User[];
    mention_roles: string[];
    mention_channels?: PartialChannel[];
    attachments: Attachment[];
    embeds: Embed[];
    reactions?: Reaction[];
    nonce?: number | string;
    pinned: boolean;
    webhook_id?: string;
    type: number;
    activity?: MessageActivity;
    application?: IntegrationApplication;
    application_id?: string;
    flags: number;
    message_reference?: MessageReference;
    referenced_message?: Message | null;
    message_snapshots?: MessageSnapshot[];
    call?: MessageCall;
    interaction?: MessageInteraction;
    interaction_metadata?: MessageInteractionMetadata;
    resolved?: ResolvedData;
    thread?: Channel;
    role_subscription_data?: MessageRoleSubscription;
    purchase_notification?: MessagePurchaseNotification;
    gift_info?: MessageGiftInfo;
    components: Component[];
    sticker_items?: StickerItem[];
    stickers?: Sticker[];
    poll?: Poll;
    changelog_id?: string;
    soundboard_sounds?: SoundboardSound[];
    potions?: Potion[];
    shared_client_theme?: SharedClientTheme;
  }

  /// Message object extra fields, gateway/gateway-events. `channel_type` is Channel Type.
  interface GatewayMessage extends Message {
    channel_type: number;
    guild_id?: string;
    member?: PartialGuildMember;
    mentions: MentionedUser[];
    metadata?: Record<string, string>;
    moderation_metadata?: Record<string, string>;
  }

  interface MessageDelete {
    id: string;
    channel_id: string;
    guild_id?: string;
  }

  /// The parts of a `GUILD_CREATE` guild that plugins read.
  interface GatewayGuild {
    id: string;
    emojis?: Emoji[];
    channels?: Channel[];
  }

  interface GuildEmojisUpdate {
    guild_id: string;
    emojis: Emoji[];
  }

  interface MessageDeleteBulk {
    ids: string[];
    channel_id: string;
    guild_id?: string;
  }

  const sig: unique symbol;

  /// An opaque hook point handed out by the runtime. `P` is the payload its hooks receive.
  interface Sig<P> {
    readonly [sig]: P;
  }

  /// Partial READY, topics/gateway-events#ready. Phoenix applies it outside the registry, so its
  /// `after` hooks run before the app has applied it, and a hook cannot drop it.
  interface Ready {
    v: number;
    user: CurrentUser;
    session_id: string;
  }

  /// Payload types for gateway events, keyed like `phoenix.gateway`.
  interface GatewayPayloads {
    ready: Ready;
    userUpdate: CurrentUser;
    messageCreate: GatewayMessage;
    /// A full message per the docs, with `tts` always false.
    messageUpdate: GatewayMessage;
    messageDelete: MessageDelete;
    messageDeleteBulk: MessageDeleteBulk;
    guildCreate: GatewayGuild;
    guildEmojisUpdate: GuildEmojisUpdate;
  }

  /// Outgoing Voice State Update payload (gateway op 4).
  interface VoiceStateUpdate {
    guild_id: string | null;
    channel_id: string | null;
    self_mute: boolean;
    self_deaf: boolean;
    self_video?: boolean;
    flags?: number;
    preferred_region?: string | null;
    preferred_regions?: string[];
    [key: string]: unknown;
  }

  /// Outgoing gateway `d` payload types by opcode, for `phoenix.gateway.onSend`.
  interface GatewaySendPayloads {
    3: Record<string, unknown>;
    4: VoiceStateUpdate;
  }

  interface GatewaySendApi {
    /// Intercepts outgoing gateway frames for numeric opcode `op` (desktop only).
    /// Receives the frame's `d` payload; return a replacement payload, `false` to
    /// drop the frame, or nothing to pass the (possibly mutated) payload on.
    /// Identify (2) and Resume (6) carry credentials and cannot be hooked.
    onSend<Op extends keyof GatewaySendPayloads>(
      op: Op,
      handler: (payload: GatewaySendPayloads[Op]) => GatewaySendPayloads[Op] | false | void
    ): Remover;
    onSend<P = unknown>(
      op: number,
      handler: (payload: P) => P | false | void
    ): Remover;
  }

  /// One handle per entry in the client's gateway registry, read from the
  /// running binary. Events without a payload type here are `Sig<unknown>`, and
  /// any event can be missing from a given build.
  type Gateway = GatewaySendApi & { readonly [E in keyof GatewayPayloads]: Sig<GatewayPayloads[E]> } & {
    readonly [event: string]: Sig<unknown> | undefined;
  };

  interface Hooks<P> {
    /// Runs before the client's handler. Return a payload to replace it, `false` to drop the
    /// event, or nothing to pass the (possibly mutated) payload on. The replacement is
    /// re-serialized with JSON.stringify, so integers above 2^53 lose precision.
    before?(payload: P): P | false | void;
    /// Runs after the client's handler with the payload it saw. `ok` is false when the handler
    /// returned an error.
    after?(payload: P, ok: boolean): void;
  }

  interface Log {
    debug(...args: unknown[]): void;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
  }

  /// Per-plugin JSON storage, written to disk on every change.
  interface Storage {
    get<T = unknown>(key: string): T | undefined;
    set(key: string, value: unknown): void;
    delete(key: string): void;
    keys(): string[];
  }

  interface SpecBase {
    title: string;
    description?: string;
  }

  type SettingSpec =
    | (SpecBase & { type: "switch"; default?: boolean; onChange?(value: boolean): void })
    | (SpecBase & {
        type: "text";
        default?: string;
        /// Edited over several lines, for lists like one rule per line. Desktop only for now.
        multiline?: boolean;
        onChange?(value: string): void;
      })
    /// Text shown masked, and replaced with `[redacted]` in the plugin's log lines.
    | (SpecBase & { type: "secret"; default?: string; onChange?(value: string): void })
    /// One of a fixed list, shown as rows with a checkmark. Defaults to the first option.
    | (SpecBase & {
        type: "select";
        options: readonly { value: string; label?: string }[];
        default?: string;
        onChange?(value: string): void;
      })
    | (SpecBase & { type: "number"; default?: number; onChange?(value: number): void })
    | (SpecBase & { type: "button"; onPress(): void });

  /// The value a setting reads as. Buttons have none.
  type SettingValue<S> = S extends { type: "switch" }
    ? boolean
    : S extends { type: "select"; options: readonly { value: infer V }[] }
      ? V
      : S extends { type: "text" | "secret" }
      ? string
      : S extends { type: "number" }
        ? number
        : never;

  type SettingValues<S> = {
    readonly [K in keyof S as S[K] extends { type: "button" } ? never : K]: SettingValue<S[K]>;
  };

  interface Settings {
    /// Shows the settings on the plugin's page in Settings. Each property of the result reads
    /// the user's current choice, so read it when needed instead of caching it. Text and number
    /// fields save when the keyboard's go key is pressed.
    define<const S extends Record<string, SettingSpec>>(schema: S): SettingValues<S>;
  }

  interface Api {
    readonly gateway: Gateway;
    /// Observes a hook point after the client handled it.
    on<P>(sig: Sig<P>, listener: (payload: P) => void): void;
    hook<P>(sig: Sig<P>, hooks: Hooks<P>): void;
    /// Feeds `payload` to the client's own handler for `sig`, skipping every plugin. Works only
    /// while a gateway hook runs, and throws elsewhere. Returns whether the handler accepted it.
    dispatch<P>(sig: Sig<P>, payload: Partial<P>): boolean;
    /// Like `dispatch`, for use outside a gateway hook. The payload reaches the client's handler
    /// just before the next gateway event it handles, usually well under a second later.
    enqueue<P>(sig: Sig<P>, payload: Partial<P>): void;
    readonly messages: Messages;
    readonly ui: Ui;
    readonly commands: Commands;
    readonly log: Log;
    readonly storage: Storage;
    readonly settings: Settings;
    /// Read-only SQL over Phoenix's own cache (`files/store/<account>/cache.sqlite`), at most
    /// 10000 rows. Arguments bind to `?` as text.
    readonly stores: Stores;
    /// Runs when the plugin is turned off, reloaded or removed. Hooks and settings go away on their
    /// own, anything else the plugin started is its to undo here.
    onUnload(cleanup: () => void): void;
    fetch(url: string, init?: FetchInit): Promise<FetchResponse>;
    /// Discord's API through the app's own HTTP client, with the app's session headers. Paths are
    /// relative to the API base, like `/users/@me`. Rejects until the app has made a request of its
    /// own.
    readonly discord: Discord;
    /// Raw access to Phoenix's Android host modules (`GuestHost.invoke`).
    readonly bridge: Bridge;
  }

  interface Bridge {
    /// Invokes a native Phoenix host module method by its `hh.jb` numeric IDs (see
    /// `docs/phoenix-356207.md`). Keys in `args` may be numeric `hh.jb` field IDs as strings
    /// (e.g. `"421"`) or known Phoenix prop names.
    invoke(
      moduleId: number,
      methodId: number,
      args?: Record<string, unknown>,
      body?: string
    ): boolean;
  }

  interface AuthorizeOptions {
    clientId: string;
    redirectUri: string;
    /// `["identify"]` by default.
    scopes?: string[];
    /// `"code"` by default.
    responseType?: string;
  }

  interface RequestFilter {
    /// Case-insensitive HTTP method filter (e.g. `"POST"`), checked in Rust.
    method?: string;
    /// Prefix match on the API-relative path (query stripped), checked in Rust.
    pathPrefix?: string;
    /// Suffix match on the API-relative path (query stripped), checked in Rust.
    pathSuffix?: string;
    /// Substring match on the full API-relative path, checked in Rust.
    pathIncludes?: string;
  }

  interface InterceptedRequest {
    readonly method: string;
    /// Path relative to the API base (e.g. `"/channels/123/messages"`).
    readonly path: string;
    /// UTF-8 request body (empty string if none). Session headers are never exposed.
    readonly body: string;
    /// Parses `body` as JSON, or returns `null` if empty/invalid.
    json<T = unknown>(): T | null;
  }

  type RequestHookResult = false | { body: string | object } | void;

  interface InterceptedResponse {
    readonly method: string;
    /// Path relative to the API base (e.g. `"/users/123/profile"`).
    readonly path: string;
    /// HTTP status code (e.g. `200`).
    readonly status: number;
    /// UTF-8 response body.
    readonly body: string;
    /// Parses `body` as JSON, or returns `null` if empty/invalid.
    json<T = unknown>(): T | null;
  }

  type ResponseHookResult = { status?: number; body?: string | object } | void;

  interface Discord {
    /// Opens Discord's own authorize screen for an OAuth2 application. Once the user taps
    /// Authorize, resolves to the redirect URL (`redirectUri?code=…`) for the plugin to fetch
    /// itself; the app shows Discord's "authorized" page instead of opening the redirect, so no
    /// browser is involved. Rejects when the user doesn't authorize within 10 minutes or another
    /// `authorize` starts. The URL holds a credential, so don't log it.
    authorize(options: AuthorizeOptions): Promise<string>;
    /// Intercepts outgoing Discord API calls matching `filter` (evaluated in Rust so non-matching
    /// calls never enter JS). Return `false` to drop the request, `{ body }` to replace its body,
    /// or nothing to pass it through unchanged.
    onRequest(
      filter: RequestFilter,
      handler: (req: InterceptedRequest) => RequestHookResult
    ): Remover;
    /// Intercepts incoming Discord API responses matching `filter` (evaluated in Rust so
    /// non-matching replies never enter JS). May return `{ body }` or `{ status, body }` to
    /// change the status too (desktop only: mobile keeps the reply's status), synchronously or as
    /// a Promise; runs off Phoenix's UI thread with a 4s fallback to the original reply.
    onResponse(
      filter: RequestFilter,
      handler: (res: InterceptedResponse) => ResponseHookResult | Promise<ResponseHookResult>
    ): Remover;
    /// The signed-in user from the app's cache, or null before the first login has synced.
    me(): Promise<CurrentUser | null>;
    request(path: string, init?: FetchInit): Promise<FetchResponse>;
    get(path: string, init?: FetchInit): Promise<FetchResponse>;
    delete(path: string, init?: FetchInit): Promise<FetchResponse>;
    post(path: string, body?: string | object, init?: FetchInit): Promise<FetchResponse>;
    put(path: string, body?: string | object, init?: FetchInit): Promise<FetchResponse>;
    patch(path: string, body?: string | object, init?: FetchInit): Promise<FetchResponse>;
  }

  interface LocalOptions {
    /// Shown as the sender, "Phoenix" by default.
    author?: string;
    guildId?: string;
    embeds?: Embed[];
  }

  interface SendAttachment {
    id: string;
    filename: string;
    uploaded_filename?: string;
    description?: string;
    [key: string]: unknown;
  }

  interface SendPayload {
    content?: string;
    attachments?: SendAttachment[];
    [key: string]: unknown;
  }

  /// Messages shown only on this device. Nothing reaches Discord, and they land just before the
  /// next gateway event Phoenix handles.
  interface Messages {
    /// Returns the new message's id.
    addLocal(channelId: string, content: string, options?: LocalOptions): string;
    updateLocal(message: Partial<Message> & { id: string; channel_id: string }): boolean;
    deleteLocal(channelId: string, id: string): boolean;
    /// Runs before a message from the message box is sent, after any slash command. Return a
    /// string to send that instead, `false` to cancel, or nothing to leave it.
    onSend(interceptor: (text: string, ctx: UiContext) => string | false | void): Remover;
    /// Runs when a custom emoji is pressed in the emoji picker under the message box (not the
    /// reaction picker), with its id. Return text to add to the end of the message box instead of
    /// what Phoenix would do (insert it, or show a Nitro ad for a locked one), or nothing to leave
    /// the press to Phoenix. While a handler exists, Phoenix counts emojis that need Nitro as
    /// usable in the message box, so its picker and autocomplete show them without a lock.
    /// Reactions and the rest keep the lock, and nothing else about Nitro changes. Desktop has no
    /// picker presses to report: the handler is never called there, only the unlock applies.
    onEmojiPick(handler: (emojiId: string, ctx: UiContext) => string | void): Remover;
  }

  /// `channelId` is the open channel, `reply` posts a local message there.
  interface UiContext {
    channelId: string | null;
    /// The signed-in user's snowflake id, when available from Phoenix's cache.
    currentUserId: string | null;
    reply(text: string): string;
  }

  interface MessageContext extends UiContext {
    messageId: string;
    /// The message as Phoenix cached it, when it did.
    message?: Message;
  }

  type NodeMatch = string | { exact: string } | { prefix: string };

  interface NodeInfo {
    readonly id: number;
    /// Phoenix `ViewKind` when Remote Debug Shell is enabled, else `null`.
    readonly kind: string | null;
    readonly accessibilityId: string | null;
    readonly parent: number | null;
    readonly children: number[];
    readonly sheet: number | null;
    readonly list: number | null;
    readonly on: string[];
  }

  interface MountNode {
    /// Phoenix `ViewKind` (e.g. `"view"`, `"text"`, `"image"`, `"banner"`, `"sheet"`).
    kind?: string;
    /// Phoenix props (`padding`/`margin` accept `[left, top, right, bottom]` or a number).
    props?: Record<string, unknown>;
    children?: MountNode[];
    /// Tap handler (`on: ["press"]` is added automatically).
    onPress?(): void;
    /// Dismiss handler (for overlays like `"sheet"`).
    onDismiss?(): void;
  }

  interface MountOptions {
    before?: "start" | "end" | number;
  }

  interface ViewEvent {
    readonly node: number;
    readonly event: string;
    readonly payload: unknown;
    readonly accessibilityId: string | null;
    /// Resolved when the event carries a row `index` pointing at a message row.
    readonly messageId?: string;
    readonly channelId?: string | null;
    readonly currentUserId?: string | null;
    readonly message?: Message;
    /// Desktop, on `rowDoubleTap`: presses the row's message menu item `action`, like
    /// `"message.actions.reply"`, without showing the menu.
    readonly press?: (action: string) => void;
  }

  interface PropPatchOptions {
    /// `"node"` (default) patches the matching node, `"sheet"` patches its enclosing sheet and
    /// wrapper, `"list"` patches its enclosing virtualized list.
    target?: "node" | "sheet" | "list";
    /// Remove this rule automatically after its first match.
    once?: boolean;
    /// Auto-expire this rule after `timeoutMs` milliseconds.
    timeoutMs?: number;
  }

  interface ProfileContext extends UiContext {
    /// Null for profiles with a default avatar.
    userId?: string;
  }

  /// An SVG icon: its shape is filled with `color` (32-bit ARGB), whatever fills the source has.
  /// Android and iOS draw no SVG and show the image at `uri` (PNG, WebP or a `data:` URI) as it is,
  /// so it carries its own color there.
  interface RenderedIcon {
    svg: string;
    color?: number;
    uri?: string;
  }

  /// Text a `render` returns: plain, colored (32-bit ARGB), or nothing. Under a message's content,
  /// `tint` also colors the message's author and text (32-bit ARGB); `text` may then be empty.
  /// `icons` are drawn in place of `text` where the client can draw them (member and header
  /// decorations), and `text` shows everywhere else.
  type Rendered =
    | string
    | { text?: string; color?: number; tint?: number; icons?: RenderedIcon[] }
    | null
    | undefined
    | false;

  /// Undoes a registration. `update` draws it again, asking the plugin anew; for a message
  /// accessory, `update(messageId)` redraws only that message, and for a profile tab or a member
  /// decoration, `update(userId)` only that user's.
  interface Remover {
    (): void;
    update?(id?: string): void;
  }

  /// A pressable piece of text, or plain text without `onPress`. `color` (32-bit ARGB) wins over
  /// `danger`.
  interface ProfileAction {
    label: string;
    danger?: boolean;
    color?: number;
    onPress?(): void;
  }

  /// What a profile tab shows, top to bottom. Colors follow the profile's theme.
  type ProfileBlock =
    /// A paragraph, with an optional heading above it.
    | { type: "text"; text: string; title?: string; color?: number }
    /// A post by someone, laid out like a chat message: avatar (an image URL), then the name,
    /// `badges` (image URLs) and a muted `subtitle` like a date, the text (custom emoji such as
    /// `<:name:id>` show as images), `images` under it, and a row of actions. `onLongPress` runs
    /// when the entry is held.
    | {
        type: "entry";
        avatar?: string;
        title: string;
        subtitle?: string;
        text: string;
        badges?: string[];
        images?: string[];
        actions?: ProfileAction[];
        /// A column at the entry's end, like ▲ score ▼ votes.
        side?: ProfileAction[];
        onLongPress?(): void;
      }
    /// A text box. `onSubmit` gets its text when the keyboard's send key is pressed, and the box
    /// empties. What is typed survives the page being drawn again.
    | {
        type: "input";
        placeholder?: string;
        onSubmit(text: string): void;
        /// A send button at the box's end, which submits like the send key.
        sendButton?: boolean;
      }
    | { type: "button"; label: string; danger?: boolean; onPress(): void }
    /// Buttons side by side.
    | { type: "buttons"; buttons: ProfileAction[] };

  /// A drawable in Phoenix's resources, like `res://ic_mana_puzzle_piece_plus`.
  type Icon = string;

  interface ButtonOptions {
    icon: Icon;
    label: string;
    /// Optional 32-bit ARGB icon tint.
    tint?: number;
    priority?: number;
    onPress(ctx: UiContext): void;
  }

  interface ButtonPatch {
    icon?: Icon;
    label?: string;
    /// 32-bit ARGB icon tint; pass `null` to reset to the default color.
    tint?: number | null;
  }

  interface ButtonRemover {
    (): void;
    update(patch: ButtonPatch): void;
  }

  interface PageButton {
    label: string;
    /// A `res://` drawable URI, or a message action accessibility id to borrow its icon.
    icon?: Icon;
    danger?: boolean;
    onPress(): void;
  }

  type PageSection =
    | { type: "code"; title?: string; text: string; monospace?: boolean; highlight?: boolean }
    | { type: "text"; title?: string; text: string }
    | { type: "buttons"; title?: string; buttons: PageButton[] };

  interface PageHandle {
    close(): void;
  }

  interface NotifyOptions {
    title: string;
    body?: string;
    /// A `res://` drawable URI (defaults to `res://ic_mana_puzzle_piece_plus`).
    icon?: Icon;
    /// 32-bit ARGB color for the leading icon badge (defaults to Discord Blurple).
    accent?: number;
    /// Auto-dismiss after this many milliseconds (defaults to 5000; pass 0 to stay until dismissed).
    durationMs?: number;
    /// Runs when the notification banner is tapped.
    onPress?(): void;
  }

  /// Plugin UI next to Phoenix's own. Plugin callbacks run off the UI threads, so drawn results
  /// land a moment after the screen they belong to. Everything goes away when the plugin is
  /// turned off.
  interface Ui {
    /// In the chat header, left of search.
    addHeaderButton(options: ButtonOptions): ButtonRemover;
    /// In the message box, left of the emoji button.
    addComposerButton(options: ButtonOptions): ButtonRemover;
    /// A line under each message's content, or after its timestamp with `placement: "header"`.
    /// It runs once per message and place, and the result is kept until `update`.
    addMessageAccessory(options: {
      placement?: "content" | "header";
      priority?: number;
      render(ctx: MessageContext): Rendered | Promise<Rendered>;
    }): Remover;
    /// Text or icons after a user's name in the member list and on their DM in the DM list.
    /// Absent on runtimes from before it. It runs once per user, and the result is kept until
    /// `update(userId)`.
    addMemberDecoration?(options: {
      priority?: number;
      render(ctx: { userId: string }): Rendered;
    }): Remover;
    /// Icons right after the author's name in message headers. Absent on runtimes from before it.
    /// Only `icons` are drawn. Like `addMemberDecoration`, it runs once per author and the result
    /// is kept until `update(userId)`.
    addMessageHeaderDecoration?(options: {
      priority?: number;
      render(ctx: { userId: string }): Rendered;
    }): Remover;
    /// A row in the message long-press menu, above Delete. The menu closes on press.
    addMessageAction(options: {
      label: string;
      icon?: Icon;
      danger?: boolean;
      priority?: number;
      visible?(ctx: MessageContext): boolean | Promise<boolean>;
      onPress(ctx: MessageContext): void;
    }): Remover;
    /// A section at the end of a profile's main tab.
    addProfileSection(options: {
      title: string;
      priority?: number;
      render(ctx: ProfileContext): Rendered | Promise<Rendered>;
    }): Remover;
    /// A tab of its own on profiles, after Phoenix's tabs. `render` runs when the profile opens
    /// (not when the tab is picked), and its page stays until `update`.
    addProfileTab(options: {
      title: string;
      priority?: number;
      render(ctx: ProfileContext): ProfileBlock[] | Promise<ProfileBlock[]>;
    }): Remover;
    /// Without `message`: a bottom sheet of `options` like Discord's message menu (a plain list
    /// dialog until that menu has been opened once). An option's `icon` is a drawable, or the
    /// accessibility id of a message menu row to borrow its icon, like `"message.actions.delete"`.
    /// With `message`: a dialog with the text and up to three buttons (`options[0]` is the main
    /// one). Resolves to the index picked, or -1 when it was dismissed.
    choose(options: {
      title?: string;
      message?: string;
      options?: (string | { label: string; icon?: Icon; danger?: boolean })[];
    }): Promise<number>;
    /// Shows a scrollable text/code viewer sheet (with optional syntax highlighting and action
    /// rows such as Copy). Resolves to the chosen option index, or -1 when dismissed.
    showSheet(options: {
      title?: string;
      text: string;
      monospace?: boolean;
      highlight?: boolean;
      options?: (string | { label: string; icon?: Icon; danger?: boolean })[];
    }): Promise<number>;
    /// Pushes a full-screen native Phoenix page onto the active navigator with a header,
    /// working Back button/gesture, scrollable syntax-highlighted code sections, and action buttons.
    openPage(options: { title: string; sections: PageSection[] }): PageHandle;
    /// Hides every Phoenix view matching `accessibilityId` (e.g. `"composer.gift"` or
    /// `"Record Voice Message"`) by omitting it from its parent's child list. Call the returned
    /// function to restore it. Desktop only knows `"composer.gift"`.
    hide(accessibilityId: string): Remover;
    /// Hides the count of pending friend requests (the DM list's Friends row, the inbox's Friends
    /// tab and their share of the Home button's count) or message requests (the DM list's
    /// Message Requests row) until the returned function runs. The requests themselves stay
    /// listed. Desktop only.
    hideCount?(count: "friendRequests" | "messageRequests"): Remover;
    /// Observes or intercepts any Phoenix view event (e.g. `"rowDoubleTap"`, `"press"`),
    /// filtered by event name in Rust. With `intercept: true`, return `false` to pass the event
    /// back to Phoenix; any other return consumes it.
    onEvent(
      event: string,
      handler: (ev: ViewEvent) => boolean | void,
      options?: { intercept?: boolean }
    ): Remover;
    /// Injects a raw view event to Phoenix's Rust guest (e.g. `"press"`, `"dismiss"`,
    /// `"rowLongPress"`).
    dispatch(node: number, event: string, payload?: Record<string, unknown>): boolean;
    /// Runs when a Phoenix node whose `accessibilityId` matches `match` is created.
    onNode(match: NodeMatch, handler: (node: NodeInfo) => void): Remover;
    /// Declaratively patches props synchronously inside Rust's UI batch rewriter (zero JS
    /// overhead per frame). Use `addOn` to append event names to `on` without clobbering Phoenix's
    /// existing listeners.
    patchProps(
      match: NodeMatch,
      patch: { addOn?: string[]; [prop: string]: unknown },
      options?: PropPatchOptions
    ): Remover;
    /// Sends a raw `Op::Update` with Phoenix props to `node`.
    update(node: number, props: Record<string, unknown>): void;
    /// Lists all live Phoenix node ids matching `match`.
    find(match: NodeMatch): number[];
    /// Finds the first live node at or under `root` matching `match`.
    findDown(root: number, match: NodeMatch, maxDepth?: number): number | null;
    /// Reads live tree metadata for `id` from Phoenix's mirrored view tree.
    node(id: number): NodeInfo | null;
    /// The live root Phoenix node id (`Op::Root`), or `null` if not yet seen.
    root(): number | null;
    /// Mounts a raw Phoenix UI subtree onto `parentId` (or `"root"` for the root view). Returns a
    /// function that unmounts and deletes all created nodes.
    mount(parentId: number | "root", node: MountNode, options?: MountOptions): Remover;
    /// Shows a native Discord in-app notification banner at the top of the screen. Returns a
    /// function that dismisses it early.
    notify(options: NotifyOptions): () => void;
    /// Suppresses haptic feedback across the app (host `Haptics` calls and view haptic props on
    /// mobile; no-op on desktop). Absent on runtimes from before it. Call the returned function
    /// to re-enable haptics.
    disableHaptics?(): Remover;
    /// Copies `text` to the device clipboard.
    copy(text: string): void;
    /// A short message at the bottom of the screen, or in a banner at the top on iOS.
    toast(text: string): void;
  }

  interface CommandInfo {
    name: string;
    description: string;
    plugin: string;
  }

  /// Slash commands typed in the message box. Matching ones are listed first in Discord's `/`
  /// suggestions, and picking one fills in `/name `.
  interface Commands {
    /// `execute` runs when `/name args` is sent. Return text to send that instead, or nothing
    /// (or a promise) to send nothing.
    register(options: {
      name: string;
      description?: string;
      icon?: Icon;
      execute(args: string, ctx: UiContext): string | { content: string } | void | Promise<unknown>;
    }): Remover;
    list(): CommandInfo[];
  }

  interface Stores {
    sql<Row = Record<string, unknown>>(query: string, ...args: unknown[]): Promise<Row[]>;
  }

  interface FetchInit {
    method?: string;
    headers?: Record<string, string>;
    /// Anything but a string is sent as JSON.
    body?: string | object;
  }

  interface FetchResponse {
    readonly status: number;
    readonly ok: boolean;
    text(): string;
    json<T = unknown>(): T;
  }
}

declare const phoenix: Phoenix.Api;
