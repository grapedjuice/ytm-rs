### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = कैश साफ़ करें?
dialog-cache-freed-size = यह { $size } को मुक्त कर देता है।
dialog-cache-frees-space = इससे जगह खाली हो जाती है।
dialog-delete-playlist-title = प्लेलिस्ट हटाएं?
dialog-delete-playlist = प्लेलिस्ट हटाएं
dialog-delete-upload-title = अपलोड हटाएं?
dialog-delete-upload = अपलोड हटाएं
dialog-name = नाम
dialog-my-playlist = मेरी प्लेलिस्ट
dialog-private-desc = निजी · केवल आप ही इसे देख सकते हैं
dialog-playlist-name = प्लेलिस्ट का नाम
dialog-description = विवरण
dialog-optional-description = एक वैकल्पिक विवरण जोड़ें
dialog-visibility = दृश्यता
dialog-private = निजी
dialog-unlisted = असूचीबद्ध
dialog-public = सार्वजनिक
dialog-visibility-prompt = अपरिवर्तित। यह बदलने के लिए कि इसे कौन देख सकता है, कोई एक चुनें।
dialog-turn-off = बंद करें
dialog-stop-collaboration = सहयोग बंद करें?
dialog-stop-collaboration-desc = सहयोगियों को प्लेलिस्ट से हटा दिया जाएगा। उनके द्वारा जोड़े गए गाने रुके रहते हैं।
dialog-collaboration-allow = सहयोगी गाने जोड़ सकते हैं।
dialog-collaboration-allow-private = सहयोगी गाने जोड़ सकते हैं। इस निजी प्लेलिस्ट को असूचीबद्ध बनाता है.
dialog-new-collaborators = नए सहयोगियों को अनुमति दें
dialog-new-collaborators-desc = नए लोगों को लिंक से जुड़ने से रोकने के लिए बंद करें।
dialog-copy-invite = आमंत्रण लिंक कॉपी करें
dialog-invite-copied = आमंत्रण लिंक कॉपी किया गया। जो भी इसे खोलेगा उसमें गाने जोड़ सकते हैं।
dialog-invite-instructions = जिन लोगों को आप जोड़ना चाहते हैं उन्हें लिंक भेजें। वे YouTube Music से जुड़ते हैं।
dialog-invites-off = नए सहयोगी शामिल नहीं हो सकते। आमंत्रण लिंक के लिए नए सहयोगियों को अनुमति दें चालू करें.
dialog-collaborators = सहयोगी
dialog-no-collaborators = अभी तक कोई शामिल नहीं हुआ है।
dialog-delete-playlist-body = “{ $title }” को आपकी YouTube Music लाइब्रेरी से हटा दिया जाएगा। इसे पूर्ववत नहीं किया जा सकता.
dialog-delete-upload-body = आपके YouTube Music अपलोड से “{ $title }” हटा दिया गया है। इसे पूर्ववत नहीं किया जा सकता.
dialog-download-count = डाउनलोड करें ({ $count })
dialog-edit-playlist = प्लेलिस्ट संपादित करें
dialog-download-body = डाउनलोड करने के लिए ट्रैक शेष हैं: { $count }। डाउनलोड पृष्ठभूमि में चलते हैं और वर्तमान ट्रैक से शुरू होते हैं।
dialog-download-collection-body = “{ $title }” से डाउनलोड करने के लिए ट्रैक शेष हैं: { $count }। डाउनलोड पृष्ठभूमि में चलते हैं और वर्तमान ट्रैक से शुरू होते हैं।
dialog-add-duplicates-title = पहले से प्लेलिस्ट में है
dialog-add-duplicates-some = इनमें से कुछ गाने पहले से “{ $title }” में हैं। फिर भी जोड़ें?
dialog-add-duplicates-body =
    { $count ->
        [one] “{ $songs }” पहले से “{ $title }” में है। फिर भी जोड़ें?
       *[other] { $more ->
            [0] ये { $count } गाने पहले से “{ $title }” में हैं: { $songs }। फिर भी जोड़ें?
           *[other] ये { $count } गाने पहले से “{ $title }” में हैं: { $songs } और { $more } और। फिर भी जोड़ें?
        }
    }
dialog-add-anyway = फिर भी जोड़ें
dialog-skip-duplicates = डुप्लिकेट छोड़ें
