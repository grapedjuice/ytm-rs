### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = 清除缓存？
dialog-cache-freed-size = 这释放了 { $size }。
dialog-cache-frees-space = 这可以释放空间。
dialog-delete-playlist-title = 删除播放列表？
dialog-delete-playlist = 删除播放列表
dialog-delete-upload-title = 删除上传内容？
dialog-delete-upload = 删除上传
dialog-name = 姓名
dialog-my-playlist = 我的播放列表
dialog-private-desc = 私人·只有你能看到
dialog-playlist-name = 播放列表名称
dialog-description = 说明
dialog-optional-description = 添加可选描述
dialog-visibility = 可见性
dialog-private = 私人
dialog-unlisted = 未列出
dialog-public = 公共
dialog-visibility-prompt = 不变。选择一个来更改谁可以看到它。
dialog-turn-off = 关闭
dialog-stop-collaboration = 关闭协作？
dialog-stop-collaboration-desc = 协作者将从播放列表中删除。他们添加的歌曲留下来。
dialog-collaboration-allow = 协作者可以添加歌曲。
dialog-collaboration-allow-private = 协作者可以添加歌曲。使该私人播放列表不公开。
dialog-new-collaborators = 允许新合作者
dialog-new-collaborators-desc = 关闭以阻止新人加入该链接。
dialog-copy-invite = 复制邀请链接
dialog-invite-copied = 已复制邀请链接。谁打开它就可以添加歌曲。
dialog-invite-instructions = 将链接发送给您要添加的人员。他们从 YouTube Music 加入。
dialog-invites-off = 新合作者无法加入。打开允许新协作者使用邀请链接。
dialog-collaborators = 合作者
dialog-no-collaborators = 尚未有人加入。
dialog-delete-playlist-body = “{ $title }”将从您的 YouTube Music 库中删除。这无法撤消。
dialog-delete-upload-body = “{ $title }”已从您上传的 YouTube Music 中删除。这无法撤消。
dialog-download-count = 下载({ $count })
dialog-edit-playlist = 编辑播放列表
dialog-download-body = 尚待下载的曲目：{ $count }。下载在后台运行并从当前曲目开始。
dialog-download-collection-body = 剩余曲目可从“{ $title }”下载：{ $count }。下载在后台运行并从当前曲目开始。
dialog-add-duplicates-title = 已在播放列表中
dialog-add-duplicates-some = 其中一些歌曲已在 “{ $title }” 中。仍然添加吗？
dialog-add-duplicates-body =
    { $count ->
       *[other] { $more ->
            [0] 这 { $count } 首已在 “{ $title }” 中：{ $songs }。仍然添加吗？
           *[other] 这 { $count } 首已在 “{ $title }” 中：{ $songs }，另有 { $more } 首。仍然添加吗？
        }
    }
dialog-add-anyway = 仍然添加
dialog-skip-duplicates = 跳过重复
