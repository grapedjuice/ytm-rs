### Settings: the page's categories, sections, rows and their descriptions.
###
### Each line is a message: its id, then `=`, then the text. Translate the text only.
### Keep { $name }-style variables as they are; they can go anywhere in the sentence.
### The descriptions under a setting say what it does in a sentence or two; buttons and
### row titles have less room, so keep those short.

## The page and its categories

settings-playback = 播放
settings-lyrics = 歌词
settings-appearance = 外观
settings-connections = 连接
settings-account = 账号
settings-general = 常规
settings-keyboard-shortcuts = 键盘快捷键
settings-about = 关于
settings-no-match = 没有与“{ $query }”匹配的设置
settings-title = 设置
settings-search-box = 搜索设置
settings-clear = 清除

## Account

settings-signed-in-desc = 使用 Google 登录。您的主页、图书馆和点赞都来自此帐户。在 Windows 凭证管理器中加密保存。
settings-sign-out = 退出
settings-channel = 频道
settings-channel-desc = 您的 Google 帐户及其管理的每个 YouTube 频道都有自己的库、喜欢、历史记录和推荐。
settings-not-signed-in = 未登录
settings-not-signed-in-desc = 登录以获取您的主页、图书馆和点赞。
settings-sign-in-with-google = 使用 Google 登录
settings-sign-in-finish = 在打开的窗口中完成登录。
settings-no-channel = { $name }（无通道）
settings-another-channel = 另一个频道
settings-channel-not-during-transfer = 传输运行时不会
settings-switching = 切换…

## Playback

settings-autoplay = 自动播放
settings-autoplay-desc = 当队列结束时继续播放类似的歌曲。
settings-crossfade-songs = 淡入淡出歌曲
settings-crossfade-songs-desc = 将每首歌曲的结尾融合到下一首歌曲的开头。
settings-crossfade-length = 淡入淡出长度
settings-crossfade-when-skipping = 跳过时交叉淡入淡出
settings-crossfade-when-skipping-desc = 当您向前跳时，也会淡入下一首歌曲。返回或选择一首歌曲仍然会立即切换。
settings-hide-explicit-songs = 隐藏露骨歌曲
settings-hide-explicit-songs-desc = 将标记为 E 的歌曲保留在列表、搜索、队列和自动播放之外。那些已经玩完了。
settings-show-podcasts = 显示播客
settings-show-podcasts-desc = 播客节目和剧集：首页、搜索结果以及您保存在媒体库中的播客过滤器。
settings-show-comments = 显示评论
settings-show-comments-desc = 正在播放的“评论”选项卡，位于“下一个”和“歌词”旁边：人们对这首歌的评价（热门或最新）以及回复。登录后，您还可以评论、回复和点赞。
settings-use-youtubes-restricted-mode = 使用YouTube的受限模式
settings-use-youtubes-restricted-mode-desc = 更严格：YouTube 还忽略了它认为成熟的其他任何内容，而不仅仅是露骨的歌曲。适用于从现在开始加载的页面。

## Song ratings

settings-song-ratings = 歌曲评分
settings-ratings-player-button = 播放器中的“不喜欢”按钮
settings-ratings-player-button-desc = 在心形旁边显示大拇指向下按钮。
settings-ratings-list-icons = 歌曲列表中的评级图标
settings-ratings-list-icons-desc = 在持续时间旁边显示评分。将鼠标悬停在未评级的歌曲上即可显示其“赞”按钮。
settings-ratings-dim-disliked = 昏暗不喜欢的歌曲
settings-ratings-dim-disliked-desc = 在歌曲列表中以柔和的颜色显示不喜欢的歌曲。
settings-ratings-skip-disliked = 不喜欢跳到下一首
settings-ratings-skip-disliked-desc = YouTube Music确认不喜欢后前进。 Listen中的嘉宾一起跟随主持人的播放。
settings-ratings-dislike-notices = 不喜欢通知
settings-ratings-dislike-notices-desc = 保存或删除不喜欢的内容时显示消息。
settings-ratings-unlike-notices = 喜欢删除通知
settings-ratings-unlike-notices-desc = 在将删除发送到 YouTube Music 之前显示撤消按钮。
settings-ratings-safe-likes = 安全喜欢的歌曲
settings-ratings-safe-likes-desc = 按住“赞”按钮可删除“赞”。
settings-ratings-hold-to-remove = 按住可删除类似

## Sound

settings-sound = 声音
settings-volume-normalization = 音量标准化
settings-logarithmic-volume = 对数体积
settings-logarithmic-volume-desc = 将音量滑块放置在耳朵听到响度的位置，这样安静的水平就不会被挤压到前几个像素中。
settings-high-quality-audio = 高品质音频
settings-high-quality-audio-desc = 对于 YouTube Premium 帐户，速度为 256 kbps，而不是 128 kbps。从下一首歌曲开始应用。
settings-output-device = 输出设备
settings-output-device-desc = 音乐播放的地方。未连接的选定设备将保留 Windows 的默认设置，直到它返回为止。
settings-windows-default = Windows 默认
settings-not-connected = { $name }（未连接）
settings-normalization-off-desc = 歌曲播放时的声音与制作时一样大声。 “正常”使它们全部达到同一水平，而“安静”则使它们达到相同的较低水平。
settings-normalization-normal-desc = 歌曲以相同的音量播放：大声的歌曲被调低，如 YouTube Music，而安静的歌曲则调高 6 dB。
settings-normalization-quiet-desc = 歌曲以相同的较低音量播放：大声的歌曲进一步降低，因此安静的歌曲很少需要调高。
settings-headphone-crossfeed = 耳机交叉馈送
settings-headphone-crossfeed-desc = 让每只耳朵都能听到另一侧的一点低音，例如来自您前面的扬声器的低音，这样通过耳机将很难移至一侧的歌曲更容易被耳朵听到。
settings-mono-audio = 单声道音频
settings-mono-audio-desc = 在每只耳朵中播放两个频道：用于一只耳塞，或用一只耳朵聆听。
settings-balance = 平衡
settings-balance-desc = 将声音移向左耳或右耳。总体而言，它的声音保持不变：它移动的一侧变得更响亮，而另一侧则变得更安静。
settings-centre = 中心

## Equalizer

settings-equalizer = 均衡器
settings-equalizer-desc = 将声音塑造为从深沉的低音到高音。只有您听到的内容会发生变化：下载或其他人听到的内容不会发生变化。
settings-preset = 预设
settings-custom = 自定义
settings-equalizer-preset = 均衡器预设
# Not shown: words a search in Settings finds the equalizer by.
settings-equalizer-search-words = 均衡器频段低音中高音频率
settings-prevent-clipping = 防止削波
settings-prevent-clipping-desc = 将所有声音调低到乐队增强的程度，这样大声的歌曲就不太可能发出噼啪声。
settings-0-db = 0 分贝

## Playback speed

settings-playback-speed = 播放速度
settings-playback-speed-desc = 也可以通过播放器栏中的按钮更快或更慢地播放歌曲。更快的声音更高，更慢的声音更低，就像唱片一样。歌词停留在时间上。每次ytm-rs启动时返回1×。
settings-speed = 速度
settings-playback-speed-jam-note = 当你陷入困境时恢复正常，这样每个人都会听到同一首歌。

## Music videos

settings-music-videos = 音乐视频
settings-songs-not-videos = 播放歌曲，而不是音乐视频
settings-songs-not-videos-desc = 当 YouTube Music 有一个官方音乐视频时，它会播放其歌曲（专辑录音）：没有前奏、小品或纯视频编辑。列表仍然显示它们所持有的内容。
settings-show-music-videos = 显示音乐视频
settings-show-music-videos-desc = 音乐视频作为移动视频播放，有自己的声音，没有歌词。歌曲原位播放时关闭，歌曲在播放时没有歌曲和视频开关；音乐视频保留他们的。
settings-video-quality = 视频质量
settings-video-quality-desc = 音乐视频播放得如此锐利，或者视频播放时最接近它。 1080p 的工作量大约是 720p 的三倍，速度较慢的计算机可能无法跟上。
settings-ambient-light = 环境光
settings-ambient-light-desc = 现在播放带有视频周围颜色的发光效果。
settings-animated-covers = 动画封面
settings-animated-covers-desc = 某些专辑的封面会移至“正在播放”，其中 Apple Music 提供了这些专辑的剪辑。找到它们会将每首歌曲的标题和艺术家发送到 Apple Music。
settings-hide-videos-in-search = 隐藏搜索中的视频
settings-hide-videos-in-search-desc = 搜索结果中仅包含歌曲、专辑、艺术家和播放列表。
settings-skip-non-music-parts = 跳过非音乐部分
settings-skip-non-music-parts-desc = 跳过音乐视频中的谈话、短剧和赞助商中断，如 SponsorBlock 社区所标记。专辑里的歌曲都没有。

## Lyrics

settings-romanization = 拉丁字母转写
settings-romanization-desc = 日语、韩语、中文、西里尔字母和其他拉丁字母文字。当来源有歌词时，与歌词同步。
settings-translation = 翻译
settings-translation-desc = 优先使用歌词自带的译文。否则由 Unison 翻译，失败时使用 Google。
settings-translate-into = 翻译语言
settings-text-size = 文字大小
settings-text-size-desc = 放大或缩小歌词。在主题本身的大小之上工作。
settings-larger = 更大
settings-smaller = 较小
settings-quick-line-changes = 快速切换歌词行
settings-quick-line-changes-desc = 下一行快速卡入到位，Better Lyrics 方式。关闭后，每个主题都按照自己的节奏移动。
settings-lyrics-full-frame-rate = 屏幕刷新率下的歌词
settings-lyrics-full-frame-rate-desc = 同步歌词的动画效果与屏幕刷新的速度一样快，例如 144 Hz 显示器上的帧速率为 144 fps，而不是 60。在快速屏幕上更流畅，可用于更多的 CPU 和图形工作。

## Lyrics timing

settings-lyrics-timing = 歌词时间轴
settings-timing-offset = 时序偏移
settings-timing-offset-desc = 将所有歌词向后移动 (+) 或向前移动 (−)。
settings-word-synced-trim = 字同步修剪
settings-word-synced-trim-desc = 音节和词同步歌词的额外转换。
settings-line-synced-trim = 线同步修剪
settings-line-synced-trim-desc = 行同步歌词的额外移位。
settings-later = 稍后
settings-earlier = 早些时候

## Lyrics sources

settings-lyrics-sources = 歌词来源
settings-lyrics-sources-note = 按此顺序尝试。关闭其中一项即可跳过它；歌词下方的工具栏会为单首歌曲选择不同的歌词。
settings-lower = 降低
settings-higher = 更高

## Appearance

settings-mode-desc-windows = 任何主题的浅色或深色页面。系统遵循Windows的应用程序模式。现在播放仍然是黑暗的，在专辑封面上。
settings-mode-desc = 任何主题的浅色或深色页面。系统遵循您的系统设置。现在播放仍然是黑暗的，在专辑封面上。
settings-mode = 模式
settings-accent-desc-file = 遵循 theme.conf 中的颜色：重音、文本以及背景（如果您愿意）。保存文件，应用程序将在几秒钟内运行。 wallust 或 pywal 也可以写。
settings-accent-desc-windows = 主题的颜色、您的 Windows 强调色、颜色文件或您选择的颜色。
settings-accent-desc = 主题的颜色、颜色文件或您选择的颜色。
settings-accent-colour = 强调色
settings-edit-file = 编辑文件
settings-custom-accent-colour = 自定义强调色
settings-accent-coloured-controls = 强调色控件
settings-accent-coloured-controls-desc = 播放按钮和音量滑块采用强调色，就像进度线一样。
settings-always-show-seek-knob = 始终显示搜索旋钮
settings-always-show-seek-knob-desc = 进度线上的旋钮保持向上状态，不仅是当您指向它时，在有进度线上的主题中也是如此。
settings-animated-background = 动画背景
settings-animated-background-desc = 专辑艺术在背景移动的主题中慢慢地漂移到一切后面。
settings-flat-background = 纯色背景
settings-flat-background-desc = 页面和正在播放后面只用一种颜色，没有专辑封面，也没有渐变。
settings-flat-background-colour = 背景颜色
settings-frosted-glass = 磨砂玻璃
settings-frosted-glass-desc = 在使用菜单和浮动栏的主题中，菜单和浮动栏会模糊其后面的内容。关闭，它们更坚固一些。
settings-pulse-with-the-music = 与音乐一起脉动
settings-pulse-with-the-music-desc = 在背景移动的主题中，背景随着节拍而膨胀。
settings-dim-bright-covers = 昏暗明亮的封面
settings-dim-bright-covers-desc = 当专辑封面明亮时使背景变暗，以便其上的歌词和标题易于阅读。深色封面保持原样。
settings-font = 字体
settings-font-desc = 对于整个应用程序和歌词。此电脑上安装的任何字体都可以使用，例如专为您的语言而设计的。
settings-themes = 主题
settings-themes-note = 从 Better Lyrics 社区主题移植。每一个都重新设计了背景、歌词、封面、播放器栏和应用程序的其余部分，上面的每一个设置都适用于所有这些。
settings-zoom = 缩放
settings-zoom-desc = 使一切变大或变小。 Ctrl + 加号和 Ctrl + 减号执行相同操作，Ctrl + 0 返回到 100%。
settings-window-material-desc = Windows 11 的材料位于页面后面，代替主题背景。
settings-window-material-desc-needs-opengl = Windows 11 的材料在页面后面。需要 OpenGL，而不是 Vulkan。
settings-window-material = 窗户材质
settings-window-material-restart-desc = 窗口从下一次开始时获取新材料。
settings-themes-font = 主题的字体
settings-installed-on-this-pc = 已安装在此电脑上
settings-looking-for-fonts = 正在寻找字体...

## Mini player

settings-pill-mini-player-desc = 实验性的。一个小药丸，当您指向它时就会显示其控件。双击它可查看完整窗口。
settings-pill-mini-player-desc-needs-opengl = 实验性的。一个小药丸，当您指向它时就会显示其控件。需要 OpenGL，而不是 Vulkan。
settings-mini-player = 迷你播放器
settings-pill-mini-player = 药丸迷你播放器
settings-keep-on-top = 保持领先
settings-keep-on-top-desc = 迷你播放器位于其他窗口之上。
settings-pill-mini-player-restart-desc = 迷你玩家从下一次开始就会改变风格。

## App icon

settings-app-icon-note-linux = 更改应用程序菜单、窗口和托盘中的图标
settings-app-icon-note = 更改窗口、任务栏、托盘和开始菜单上的图标
settings-app-icon = 应用程序图标
settings-preview = 预览
settings-preview-desc = 它在任务栏和托盘中的外观。
settings-app-icon-named = 应用程序图标：{ $name }

## Connections

settings-stream-deck = Stream Deck
settings-stream-deck-desc = 让这台计算机上的 Stream Deck 插件控制播放。
settings-lastfm-connected = 将您所玩的内容记录到 { $name } 的个人资料中。
settings-lastfm-allow = 允许在浏览器中打开的 Last.fm 页面上使用 ytm-rs。
settings-lastfm-desc = 将您播放的内容记录到您的 Last.fm 个人资料中。
settings-disconnect = 断开连接
settings-connect = 连接
settings-listenbrainz-connected = 正在将您播放的内容发送到 { $name } 的个人资料。
settings-listenbrainz-desc = 将您玩的内容发送到您的 ListenBrainz 个人资料。
settings-user-token = 用户令牌
settings-listenbrainz-token-hint = 粘贴您的 ListenBrainz 用户令牌
settings-listenbrainz-token-link = 在 Listenbrainz.org 上获取您的令牌

## Language

# { $language } is the system's language, in its own name.
settings-language-system = 跟随系统（{ $language }）
settings-language = 语言
settings-language-desc = 应用界面的语言。歌曲标题、名称和 YouTube Music 内容分区使用下方选择的歌曲信息语言。
settings-restart-to-apply = 重启后应用
settings-language-restart-desc = 下次打开应用时将使用新的语言。
settings-restart-now = 立即重启

## Window

settings-window = 窗口
settings-open-on = 启动页面
settings-open-on-desc = 应用程序启动的页面。图书馆自己的选项卡需要您登录（下载除外）；注销后，应用程序将在主页上打开。
settings-home = 首页
settings-close-to-tray-desc-linux = 在托盘中保留图标。单击 × 隐藏窗口并保持音乐播放。图标的菜单可以将其恢复、隐藏或退出。单击系统媒体控件中的歌曲也会将其恢复，然后按 Ctrl+Q 退出。
settings-close-to-tray-desc = 在托盘中保留图标。单击 × 隐藏窗口并保持音乐播放。单击该图标可将其恢复，或右键单击该图标可隐藏、显示或退出。
settings-close-to-tray = 关闭时隐藏到系统托盘
settings-song-notifications-desc = 当窗口最小化时在托盘中或其他窗口后面开始播放新歌曲时，会在通知中显示其标题、艺术家和封面。
settings-song-notifications-unavailable = 需要安装的应用程序：Windows 仅在“开始”菜单中显示来自应用程序的通知。
settings-song-notifications = 歌曲通知
settings-close-to-tray-restart-desc = 在Wayland上，窗口只能隐藏在托盘中，并在重新启动后返回。直到那时×将其最小化。
settings-title-bar = 标题栏
settings-title-bar-desc = 谁绘制窗口的标题栏和按钮。自动使用 ytm-rs，桌面不绘制（GNOME 在 Wayland 上）。
settings-title-bar-restart-desc = 标题栏从下次开始时发生变化。
settings-collapse-sidebar = 折叠侧边栏
settings-collapse-sidebar-desc = 对于狭窄的窗口，仅在侧边栏中显示图标。顶部的菜单按钮具有相同的作用。
settings-draw-with = 用{ $api }抽奖
settings-renderer-restart-desc = 应用程序从下次开始时使用新的渲染器进行绘制。

## Downloads

settings-downloads = 下载
settings-downloads-note = 从他们的菜单下载歌曲，或从他们的页面下载整个专辑和播放列表。它们以您帐户获得的最佳质量保存，并标有封面艺术，并可以从文件中播放，甚至可以离线播放。
settings-download-folder = 下载文件夹
settings-change = 改变
settings-open = 打开

## Network

settings-network = 网络
settings-proxy = 代理
settings-proxy-desc = 发送 ytm-rs 通过代理获取的所有内容：歌曲、歌词、登录并一起收听。
settings-proxy-windows-uses = Windows 设置为使用 { $proxy }。
settings-proxy-windows-none = Windows 未设置为使用代理，因此 ytm-rs 直接连接。
settings-proxy-environment-sets = 环境设置{ $proxy }。
settings-proxy-environment-none = 没有设置HTTPS_PROXY或ALL_PROXY，所以ytm-rs直接连接。
settings-windows-proxy = Windows代理
settings-system-proxy = 系统代理
settings-proxy-address = 代理地址
settings-address = 地址
settings-saved = 已保存
settings-save = 保存
settings-proxy-password-note = 地址中的密码保存在 Windows 凭证管理器中。
settings-checking = 正在检查……
settings-proxy-test-desc = 检查 YouTube Music 是否通过此设置应答。
settings-test-connection = 测试连接
settings-test = 测试
settings-test-working = 工作：YouTube Music 在 { $ms } 毫秒内应答。
settings-test-failed = 未连接：{ $error }
settings-proxy-restart-desc = 下次启动时将使用新的代理设置。

## Storage

settings-cache-cleared = 缓存已清除：{ $size } 已释放
settings-storage = 存储
settings-clearing = 正在清理……
settings-measuring = 测量……
settings-cache = 缓存
settings-clear-cache = 清除缓存

## Keyboard shortcuts

settings-play-pause = 播放/暂停
settings-space = 空间
settings-next-previous-song = 下一首/上一首歌曲
settings-ctrl = Ctrl
settings-volume-up-down = 音量增大/减小
settings-shortcut-command-palette = 命令选项板：页面、设置、播放列表和操作
settings-search = 搜索
settings-shortcut-find-in-list = 在歌曲列表中查找
settings-shift = 转变
settings-lyrics-and-now-playing = 歌词并正在播放
settings-shortcut-full-screen = 全屏正在播放：歌词或视频
settings-shortcut-video-fill = 全屏视频：填满屏幕或适合屏幕
settings-mini-player-on-off = 迷你播放器开/关
settings-back-forward = 后退/前进
settings-alt = 替代
settings-shortcut-back-forward-mouse = 用鼠标后退/前进
settings-side-buttons = 侧面按钮
settings-refresh-the-page = 刷新页面
settings-f5-ctrl-r = F5 / Ctrl + R
settings-zoom-in-out = 放大/缩小
settings-shortcut-zoom-reset = 缩放回 100%
settings-shortcut-leave = 保持全屏，现在播放或选择
settings-esc = 退出

## Updates

settings-updates = 更新
settings-updates-dev-build = 更新仅适用于已安装或便携式应用程序（这是开发版本）。
settings-checking-for-updates = 正在检查更新...
settings-up-to-date = 您使用的是最新版本 ({ $version })。
settings-update-downloading = 正在下载{ $version }…{ $percent }%
settings-update-ready = { $version } 版本已准备就绪。它会在您关闭 ytm-rs 时安装。
settings-update-available = 版本 { $version } 已发布。 { $step }
settings-update-failed = 无法检查更新：{ $error }
settings-version = 版本 { $version }。
settings-automatic-updates-desc-packaged = 检查新版本并在新版本发布时通知您。
settings-automatic-updates-desc = 在后台下载新版本，并在关闭应用程序时安装它们。
settings-automatic-updates = 自动更新
settings-update-channel = 更新频道
settings-nightly = 每晚
settings-stable = 稳定
settings-status = 状态
settings-restart-to-update = 重新启动以更新
settings-download = 下载
settings-check-now = 立即检查
settings-another-copy = 另一份
settings-another-copy-desc = ytm-rs 也是从 { $kind } 安装的。要删除该副本，请运行 { $cmd }
settings-whats-new = 最新消息
settings-whats-new-in = { $version } 的新功能

## About

settings-about-tagline = 快速、本机 YouTube Music 客户端。
settings-about-lyrics-sources = Better Lyrics API：Musixmatch、LRCLIB、QQ、酷狗和 Better Lyrics TTML
settings-about-themes-credit = 改编自Better Lyrics主题商店；每张卡都归功于其创造者
settings-background = 背景
settings-about-background-credit = Kawarp 的端口，麻省理工学院 © Better Lyrics
settings-fonts-and-icons = 字体和图标
settings-about-fonts-credit = Inter、DM Sans、Google Sans Flex、Figtree、Instrument Sans (SIL OFL 1.1) • Lucide (ISC)
# { $shown } is { $count } written with its thousands separators.
settings-downloads-and-counting = 截至目前已下载 { $shown } 次
settings-counting-downloads = 正在计算下载量...
settings-fade-when-pausing = 暂停时淡出音量
settings-fade-when-pausing-desc = 暂停时在 0.2 秒内逐渐降低音量，继续播放时恢复音量。关闭后会立即暂停和继续播放。
settings-song-info = 歌曲信息
settings-song-info-language = 歌曲信息语言
settings-song-info-language-desc = 歌曲标题、歌手和专辑名称以及 YouTube Music 内容分区的语言，取决于是否有相应翻译。
settings-song-info-country = 排行榜和推荐的国家/地区
settings-song-info-country-desc = 排行榜和推荐内容所面向的国家/地区。
settings-folder-per-list = 为每个专辑和播放列表单独创建文件夹
settings-folder-per-list-desc = 从专辑或播放列表的页面或菜单下载时，会将歌曲保存在以其命名的文件夹中。已下载的歌曲保留在原位置。
settings-backup-stream = 歌曲停止时使用备用流
settings-backup-stream-desc = 登录后，如果 YouTube 为验证你是否为机器人而停止播放歌曲（使用 VPN 时可能发生），则使用包含视频的流播放。会消耗更多流量。
settings-backup-stream-prompt-title = 播放已停止
settings-backup-stream-prompt-body = YouTube 为验证你是否为机器人而停止了这首歌。使用 VPN 时可能发生。备用流可以继续播放，但它包含视频，因此会消耗更多流量。
settings-backup-stream-prompt-enable = 启用并重试
settings-backup-stream-prompt-settings = 设置
settings-backup-stream-prompt-dismiss = 暂不
