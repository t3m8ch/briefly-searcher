# Веб-лента `t.me/s`: что даёт публичная HTML-лента канала

Область: чтение истории публичного канала `@brieflyru` без аккаунта Telegram, через веб-ленту `https://t.me/s/brieflyru`. Причина: создать приложение на my.telegram.org и получить `api_id`/`api_hash` для MTProto сейчас нельзя из-за бага Telegram (см. #7, #8, PR #13).

Все наблюдения ниже — живые запросы к `t.me/s/brieflyru` 27.09.2026, когда последним сообщением канала было примерно 47 126. Официальной документации у веб-ленты нет: это HTML для браузера, и его разметка может меняться без предупреждения.

## Пагинация

- `GET https://t.me/s/brieflyru` — самые свежие посты канала.
- `GET https://t.me/s/brieflyru?before=N` — посты с ID **строго меньше** `N`, самые свежие из них. Семантика совпадает с `offset_id` у `messages.getHistory`: исключающая верхняя граница, запрос без параметра соответствует `offset_id = 0`.
- Внутри страницы посты идут **по возрастанию** ID: старые сверху, как в браузере.
- В `<head>` есть `<link rel="prev" href="/s/brieflyru?before=<наименьший ID страницы>">`, пока до начала канала есть ещё посты. Есть и `?after=N` с `rel="next"`, но загрузчику он не нужен.
- **Размер страницы — около 20 сообщений, считая каждое фото альбома.** Поэтому постов (блоков) на странице бывает от 4 до 20. Пример: `before=31170` → 4 блока-альбома с 23 фотографиями. Параметра для размера страницы нет.
- **Начало канала:**
  - `before=30` → посты 10–29;
  - `before=5` → только сообщение 1 («Channel created»), без `rel="prev"`;
  - `before=1` → пустая лента: в разметке есть блок `<section class="tgme_channel_history …">` с `<div class="tme_no_messages_found">No posts found</div>`, ссылки `rel="prev"` нет.
- **Несуществующий канал** отвечает `302` (проверено на `t.me/s/thischanneldoesnotexist12345xyz`), а не пустой лентой.

ID 2–4 в ленте не показаны. Вероятно, эти сообщения удалены, но по HTML это не определить.

## Альбомы

- Альбом показан **одним блоком** (`.tgme_widget_message` с одним `data-post`). Его ID — ID первой фотографии.
- Внутри блока каждая фотография — ссылка `<a class="tgme_widget_message_photo_wrap … " href="https://t.me/brieflyru/<ID>?single">`, так что **ID всех фото альбома видны**. Пример: блок `47109` содержит фото `47109`–`47112`.
- Отдельных блоков для остальных фото альбома в ленте нет. Все пропуски ID на проверенных страницах объясняются альбомами: на странице `before=47107` блоки `47086, 47089, …`, а `47087`–`47088` — фото альбома `47086`.
- Подпись альбома показана одна, в блоке. Подписи отдельных фото, если они есть, в ленте не видны.

## Что есть у поста

Блок — это элемент `<div class="tgme_widget_message …" data-post="brieflyru/<ID>">`:

| Что | Где в разметке |
| --- | --- |
| ID сообщения | `data-post="brieflyru/<ID>"`; ссылка даты `href="https://t.me/brieflyru/<ID>"` |
| Текст с разметкой | `.tgme_widget_message_text`: `<b>`, `<i>`, `<a>`, `<blockquote expandable>`, `<br/>` |
| Дата публикации | `<time datetime="2026-09-27T07:24:01+00:00">` в `.tgme_widget_message_date` |
| Признак правки | слово `edited` в `.tgme_widget_message_meta` |
| Просмотры | `.tgme_widget_message_views`, округлённо (`4.85K`) |
| Реакции | `.tgme_widget_message_reactions` |
| Фото | `.tgme_widget_message_photo_wrap` со ссылкой на CDN `cdn*.telesco.pe` в `background-image` (временные ссылки) |
| Служебное сообщение | класс `service_message` у блока (например, «Channel created») |

Лента показывает **текущее** состояние поста: правленный текст с пометкой `edited`, текущие просмотры и реакции.

`grouped_id`, TL-объекта сообщения и метаданных медиа (размеры, `file_id`) в ленте нет.

## Ограничение частоты запросов

Проверка: 300 последовательных запросов `?before=<случайный N от 100 до 47 000>` без пауз, по одному, примерно 7,5 минут, в среднем около 1 запроса в секунду (ответ ≈ 0,9 с).

- **288 ответов — `200` с нормальной страницей** (посты есть, `rel="prev"` есть).
- **Ни одного `429`, ни одного другого HTTP-кода, ни одной пустой или урезанной ленты.** Поэтому формат ответа-ограничения, наличие `Retry-After` и время восстановления **не установлены**.
- **12 запросов (4 %) оборвались сбросом соединения** после ~11 с ожидания (`curl: (35) Recv failure: Connection reset by peer`). Сессия шла через прокси, и его журнал фиксирует закрытие туннеля. Сбросил ли соединение Telegram или прокси, отсюда не определить. Повтор того же запроса через 5 с проходил нормально. Сбросы шли вразброс и не учащались к концу серии, на ограничение частоты это не похоже.

Выводы для загрузчика:
- при темпе около 1 запроса в секунду ограничение не срабатывает, а загрузчик будет ходить медленнее: запросы по одному с настраиваемой задержкой;
- ответ-ограничение надо обрабатывать, не зная его формата: на `429` загрузчик делает собственную паузу из конфигурации (`FloodWait`), не пытаясь читать `Retry-After`. Каждый `429` пишется в лог целиком — адрес, заголовки, начало тела, — чтобы по накопленным ответам потом сделать более умное ожидание. Что именно отвечает веб-лента при ограничении, покажет полная загрузка (#9);
- сбросы соединения и таймауты — обычная ошибка источника с повтором после паузы, не `FloodWait`;
- **пустую ленту нельзя считать началом канала по одному отсутствию постов.** Настоящее начало — ответ `200` с блоком `tgme_channel_history` и `tme_no_messages_found`. Любой другой ответ без постов (редирект, другая разметка, пустое тело) — ошибка источника. Иначе загрузчик завершил бы проход и сдвинул `newest_fetched_id` поверх незагруженной истории.

## Примеры разметки

Фрагменты сокращены: хвост пузыря (`svg`), CDN-ссылки, `data-view` и длинный текст заменены на `…`. Остальное — как в ответе.

Обычный пост с правкой:

```html
<div class="tgme_widget_message text_not_supported_wrap js-widget_message" data-post="brieflyru/47107" data-view="…">
  <div class="tgme_widget_message_user"><a href="https://t.me/brieflyru"><i class="tgme_widget_message_user_photo bgcolor2" data-content="B"><img src="https://cdn4.telesco.pe/file/…"></i></a></div>
  <div class="tgme_widget_message_bubble">
    <i class="tgme_widget_message_bubble_tail">…svg…</i>
    <div class="tgme_widget_message_author accent_color"><a class="tgme_widget_message_owner_name" href="https://t.me/brieflyru"><span dir="auto">BRIEFLY RU</span></a></div>
    <div class="tgme_widget_message_text js-message_text" dir="auto"><b><i>«С противниками нужно вести переговоры», </i>— сказал министр иностранных дел Германии…</b><br/>…<blockquote expandable>…</blockquote><br/>…<a href="https://t.me/brieflyru" target="_blank" rel="noopener" onclick="…"><b>Подписаться</b></a></div>
    <div class="tgme_widget_message_reactions js-message_reactions"><span class="tgme_reaction"><i class="emoji" style="…"><b>🤣</b></i>34</span>…</div>
    <div class="tgme_widget_message_footer compact js-message_footer">
      <div class="tgme_widget_message_info short js-message_info">
        <span class="tgme_widget_message_views">4.85K</span><span class="copyonly"> views</span><span class="tgme_widget_message_meta">edited &nbsp;<a class="tgme_widget_message_date" href="https://t.me/brieflyru/47107"><time datetime="2026-09-27T07:24:01+00:00" class="time">07:24</time></a></span>
      </div>
    </div>
  </div>
</div>
```

Альбом из четырёх фото (`47109`–`47112`) с общей подписью:

```html
<div class="tgme_widget_message text_not_supported_wrap js-widget_message" data-post="brieflyru/47109" data-view="…">
  …
  <div class="tgme_widget_message_grouped_wrap js-message_grouped_wrap" data-margin-w="2" data-margin-h="2" style="width:453px;">
    <div class="tgme_widget_message_grouped js-message_grouped" style="padding-top:133.333%">
      <div class="tgme_widget_message_grouped_layer js-message_grouped_layer" style="width:453px;height:604px">
        <a class="tgme_widget_message_photo_wrap grouped_media_wrap blured js-message_photo" style="…;background-image:url('https://cdn4.telesco.pe/file/…')" data-ratio="1.1977491961415" href="https://t.me/brieflyru/47109?single">…</a>
        <a class="tgme_widget_message_photo_wrap grouped_media_wrap blured js-message_photo" style="…" data-ratio="1.2389525368249" href="https://t.me/brieflyru/47110?single">…</a>
        <a class="tgme_widget_message_photo_wrap grouped_media_wrap blured js-message_photo" style="…" data-ratio="1.7698630136986" href="https://t.me/brieflyru/47111?single">…</a>
        <a class="tgme_widget_message_photo_wrap grouped_media_wrap blured js-message_photo" style="…" data-ratio="1.3382352941176" href="https://t.me/brieflyru/47112?single">…</a>
      </div>
    </div>
  </div>
  <div class="tgme_widget_message_text js-message_text" dir="auto"><div class="tgme_widget_message_text js-message_text" dir="auto"><b>Операция «Мёртвая рука». Строительство российского ядерного бункера «судного дня» </b>— The Sunday T…</div></div>
  …
  <span class="tgme_widget_message_meta"><a class="tgme_widget_message_date" href="https://t.me/brieflyru/47109"><time datetime="2026-09-27T08:21:14+00:00" class="time">08:21</time></a></span>
  …
</div>
```

Первое сообщение канала — служебное:

```html
<div class="tgme_widget_message text_not_supported_wrap service_message js-widget_message" data-post="brieflyru/1" data-view="…">
  …
  <div class="tgme_widget_message_text js-message_text" dir="auto">Channel created</div>
  <div class="tgme_widget_message_footer compact js-message_footer">
    <div class="tgme_widget_message_info short js-message_info">
      <span class="tgme_widget_message_meta"><a class="tgme_widget_message_date" href="https://t.me/brieflyru/1"><time datetime="2023-03-03T09:36:39+00:00" class="time">09:36</time></a></span>
    </div>
  </div>
</div>
```

## Открытые вопросы

- **Ответ при ограничении частоты.** Код, наличие `Retry-After`, время восстановления. За 300 запросов около 1 запроса в секунду ограничение не сработало; ответ покажет полная загрузка (#9).
- **Сбросы соединения.** Кто сбрасывает — Telegram или прокси сессии — и будут ли они при запуске не из облачной сессии.
- **Посты без веб-превью.** На проверенных страницах пометок «откройте в Telegram» не было, но пролистана малая часть истории.
- **ID 2–4.** Удалены или просто не показаны в ленте.
- **Подписи отдельных фото альбома** в ленте не видны; есть ли они у Briefly, по ленте не узнать.
