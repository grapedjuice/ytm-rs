### Dialog titles, buttons and explanatory text.

dialog-clear-cache-title = ล้างแคช?
dialog-cache-freed-size = สิ่งนี้ทำให้ { $size } ว่างขึ้น
dialog-cache-frees-space = ซึ่งจะทำให้มีพื้นที่ว่างมากขึ้น
dialog-delete-playlist-title = ลบเพลย์ลิสต์ใช่ไหม
dialog-delete-playlist = ลบเพลย์ลิสต์
dialog-delete-upload-title = ลบการอัปโหลดใช่ไหม
dialog-delete-upload = ลบการอัพโหลด
dialog-name = ชื่อ
dialog-my-playlist = เพลย์ลิสต์ของฉัน
dialog-private-desc = ส่วนตัว · มีเพียงคุณเท่านั้นที่สามารถเห็นได้
dialog-playlist-name = ชื่อเพลย์ลิสต์
dialog-description = คำอธิบาย
dialog-optional-description = เพิ่มคำอธิบายเพิ่มเติม
dialog-visibility = การมองเห็น
dialog-private = ส่วนตัว
dialog-unlisted = ไม่อยู่ในรายการ
dialog-public = สาธารณะ
dialog-visibility-prompt = ไม่เปลี่ยนแปลง เลือกหนึ่งรายการเพื่อเปลี่ยนผู้ที่สามารถเห็นได้
dialog-turn-off = ปิด
dialog-stop-collaboration = ปิดการทำงานร่วมกันใช่ไหม
dialog-stop-collaboration-desc = ผู้ทำงานร่วมกันจะถูกลบออกจากเพลย์ลิสต์ เพลงที่พวกเขาเพิ่มอยู่
dialog-collaboration-allow = ผู้ทำงานร่วมกันสามารถเพิ่มเพลงได้
dialog-collaboration-allow-private = ผู้ทำงานร่วมกันสามารถเพิ่มเพลงได้ ทำให้เพลย์ลิสต์ส่วนตัวนี้ไม่อยู่ในรายการ
dialog-new-collaborators = อนุญาตผู้ทำงานร่วมกันใหม่
dialog-new-collaborators-desc = ปิดเพื่อหยุดไม่ให้คนใหม่เข้าร่วมด้วยลิงก์
dialog-copy-invite = คัดลอกลิงก์คำเชิญ
dialog-invite-copied = คัดลอกลิงก์คำเชิญแล้ว ใครเปิดก็เติมเพลงได้
dialog-invite-instructions = ส่งลิงก์ไปยังบุคคลที่คุณต้องการเพิ่ม พวกเขาเข้าร่วมจาก YouTube Music
dialog-invites-off = ผู้ทำงานร่วมกันใหม่ไม่สามารถเข้าร่วมได้ เปิดอนุญาตผู้ทำงานร่วมกันใหม่สำหรับลิงก์คำเชิญ
dialog-collaborators = ผู้ร่วมงาน
dialog-no-collaborators = ยังไม่มีใครเข้าร่วมเลย
dialog-delete-playlist-body = “{ $title }” จะถูกลบออกจากไลบรารี่ YouTube Music ของคุณ การดำเนินการนี้ไม่สามารถยกเลิกได้
dialog-delete-upload-body = “{ $title }” ถูกลบออกจากการอัปโหลด YouTube Music ของคุณ การดำเนินการนี้ไม่สามารถยกเลิกได้
dialog-download-count = ดาวน์โหลด ({ $count })
dialog-edit-playlist = แก้ไขเพลย์ลิสต์
dialog-download-body = เพลงที่เหลือให้ดาวน์โหลด: { $count }. การดาวน์โหลดจะทำงานอยู่เบื้องหลังและเริ่มต้นด้วยแทร็กปัจจุบัน
dialog-download-collection-body = แทร็กที่เหลือให้ดาวน์โหลดจาก “{ $title }”: { $count } การดาวน์โหลดจะทำงานอยู่เบื้องหลังและเริ่มต้นด้วยแทร็กปัจจุบัน
dialog-add-duplicates-title = มีอยู่ในเพลย์ลิสต์แล้ว
dialog-add-duplicates-some = เพลงเหล่านี้บางเพลงมีอยู่ใน "{ $title }" แล้ว เพิ่มต่อไปไหม?
dialog-add-duplicates-body =
    { $count ->
       *[other] { $more ->
            [0] { $count } เพลงนี้อยู่ใน "{ $title }" แล้ว: { $songs } เพิ่มต่อไปไหม?
           *[other] { $count } เพลงนี้อยู่ใน "{ $title }" แล้ว: { $songs } และอีก { $more } เพลง เพิ่มต่อไปไหม?
        }
    }
dialog-add-anyway = เพิ่มต่อไป
dialog-skip-duplicates = ข้ามรายการซ้ำ
