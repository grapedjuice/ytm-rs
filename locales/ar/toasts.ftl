# After unliking several songs at once; Undo sits beside it.
likes-removed = { $count ->
    [zero] لم تتم إزالة أي إعجاب
    [one] تمت إزالة إعجاب واحد
    [two] تمت إزالة إعجابين
    [few] تمت إزالة { $count } إعجابات
    [many] تمت إزالة { $count } إعجابًا
   *[other] تمت إزالة { $count } إعجاب
}
