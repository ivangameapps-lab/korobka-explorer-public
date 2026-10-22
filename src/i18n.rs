//! Мини-локализация: русский и английский.
//!
//! Язык определяется по Windows при первом запуске, но пользователь может
//! выбрать его в настройках. Строки, для которых нет перевода, остаются
//! русскими — так проще добавлять новые места без риска пустого интерфейса.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_english(value: bool) {
    ENGLISH.store(value, Ordering::Relaxed);
}

pub(crate) fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Возвращает английский вариант, если он есть, иначе исходную строку.
pub(crate) fn tr(text: &str) -> &str {
    if !english() {
        return text;
    }
    match text {
        "Enter — поиск в папке и подпапках, лупа — весь диск" => {
            "Enter — search current folder and subfolders, magnifier — whole drive"
        }
        "ZIP-архив" => "ZIP archive",
        "Акцент" => "Accent",
        "Анимации" => "Animations",
        "Архив пуст" => "Archive is empty",
        "Архив" => "Archive",
        "Аудио" => "Audio",
        "БЫСТРЫЙ ДОСТУП" => "QUICK ACCESS",
        "Буфер обмена пуст" => "Clipboard is empty",
        "В этом ZIP нет файлов" => "This ZIP has no files",
        "Вверх (Backspace)" => "Up (Backspace)",
        "Вверх" => "Up",
        "Вид" => "View",
        "Видео" => "Video",
        "Восстановить" => "Restore",
        "Вкладки и открытые папки не сбрасываются." => {
            "Tabs and open folders are not reset."
        }
        "Восстановить их будет нельзя." => "They cannot be restored.",
        "Вперёд (Alt+→)" => "Forward (Alt+→)",
        "Вставить            Ctrl+V" => "Paste               Ctrl+V",
        "Выбор папки Korobka" => "Choose Korobka folder",
        "Выбрать эту папку" => "Use this folder",
        "Вырезать           Ctrl+X" => "Cut                 Ctrl+X",
        "Вырезать" => "Cut",
        "Готово" => "Done",
        "Действия" => "Actions",
        "ДИСКИ" => "DRIVES",
        "Диски:" => "Drives:",
        "Дождитесь завершения текущей операции" => "Wait for the current operation to finish",
        "Документ Word" => "Word document",
        "Документ" => "Document",
        "Документы" => "Documents",
        "Домашняя папка" => "Home",
        "Достаточно вхождения в имя" => "Any part of the name matches",
        "Загрузки" => "Downloads",
        "Загрузка…" => "Loading…",
        "Закрываю процессы…" => "Closing processes…",
        "Закрыть архив" => "Close archive",
        "Закрыть выбранные и удалить" => "Close selected and delete",
        "Закрыть выбранные процессы? Несохранённые данные могут быть потеряны." => {
            "Close the selected processes? Unsaved data may be lost."
        }
        "Закрыть поиск" => "Close search",
        "Закрыть свойства" => "Close properties",
        "Закрыть" => "Close",
        "Заменить" => "Replace",
        "защищён" => "protected",
        "Здесь пока нет объектов" => "No items here yet",
        "Извлечь всё" => "Extract all",
        "Извлечь…" => "Extract…",
        "Изменён" => "Modified",
        "Изображение" => "Image",
        "Изображения" => "Pictures",
        "Имя не может быть пустым" => "Name cannot be empty",
        "Имя файла должно совпасть целиком" => "File name must match exactly",
        "Имя" => "Name",
        "Изменить путь (Ctrl+L)" => "Edit path (Ctrl+L)",
        "Измените запрос в поле поиска" => "Change the search query",
        "Измените фильтр внутри архива" => "Change the filter inside the archive",
        "Индекс пересоздастся при следующем поиске." => {
            "The index will be rebuilt on the next search."
        }
        "Искать в текущей папке…" => "Search current folder…",
        "Искать на всём диске" => "Search entire drive",
        "Ищу…" => "Searching…",
        "Код" => "Code",
        "Компактные строки" => "Compact rows",
        "Компьютер" => "Computer",
        "Конфликт" => "Conflict",
        "Корень диска удалить нельзя" => "The drive root cannot be deleted",
        "Копировать        Ctrl+C" => "Copy                Ctrl+C",
        "Копировать путь" => "Copy path",
        "Копировать" => "Copy",
        "Локальный диск" => "Local disk",
        "Масштаб" => "Scale",
        "Матовый" => "Matte",
        "Назад (Alt+←)" => "Back (Alt+←)",
        "Назад" => "Back",
        "Настройки" => "Settings",
        "Настройки…" => "Settings…",
        "Настройки сохранены" => "Settings saved",
        "Настройки сброшены" => "Settings reset",
        "Не удалось найти файл в архиве" => "Could not find the file in the archive",
        "Не удалось открыть архив" => "Could not open archive",
        "Нет совпадений" => "No matches",
        "Нечего отменять" => "Nothing to undo",
        "Ничего не выбрано" => "Nothing selected",
        "Ничего не найдено" => "Nothing found",
        "Новая вкладка (Ctrl+T)" => "New tab (Ctrl+T)",
        "Новая папка        Ctrl+Shift+N" => "New folder          Ctrl+Shift+N",
        "Новая папка" => "New folder",
        "Новая таблица Excel" => "New Excel spreadsheet",
        "Новый архив" => "New archive",
        "Новый документ Word" => "New Word document",
        "Новый текстовый документ" => "New text document",
        "Новый файл" => "New file",
        "Ярлык" => "Shortcut",
        "Обновить" => "Refresh",
        "Обновить             F5" => "Refresh              F5",
        "Обновить (F5)" => "Refresh (F5)",
        "Обновить архив" => "Refresh archive",
        "Обновить список дисков" => "Refresh drives",
        "Обновить список" => "Refresh list",
        "Объект уже существует:" => "Object already exists:",
        "Оставить оба" => "Keep both",
        "Открыть в новой вкладке" => "Open in new tab",
        "Открыть с помощью…" => "Open with…",
        "Открыть" => "Open",
        "Отмена" => "Cancel",
        "Отменить                 Ctrl+Z" => "Undo                     Ctrl+Z",
        "Оформление" => "Appearance",
        "Очистить поиск" => "Clear search",
        "Очистить фильтр" => "Clear filter",
        "Папка пуста" => "Folder is empty",
        "Папки сверху" => "Folders first",
        "Папку" => "Folder",
        "Папка" => "Folder",
        "Переименовать              F2" => "Rename                     F2",
        "Переименовать" => "Rename",
        "Перейти к папке…" => "Go to folder…",
        "Перейти" => "Go",
        "Плитка" => "Tiles",
        "По возрастанию" => "Ascending",
        "По убыванию" => "Descending",
        "По части имени" => "Part of name",
        "Поиск" => "Search",
        "Показывать панель свойств" => "Show properties panel",
        "Показывать скрытые и системные" => "Show hidden and system items",
        "Полосатые строки" => "Striped rows",
        "Попробуйте другой запрос" => "Try another query",
        "Применить ко всем" => "Apply to all",
        "Программа" => "Application",
        "Прозрачность" => "Transparency",
        "Пропустить" => "Skip",
        "Проверка занятости прервана" => "Lock check interrupted",
        "Проверяю занятость…" => "Checking locks…",
        "Проверяю, что удерживает файл…" => "Checking what is locking the file…",
        "Процессы:" => "Processes:",
        "Пустой файл" => "Empty file",
        "Путь скопирован" => "Path copied",
        "Путь к папке" => "Folder path",
        "Путь" => "Path",
        "Путь:" => "Path:",
        "Рабочий стол" => "Desktop",
        "Развернуть" => "Maximize",
        "Развернуть панель" => "Expand panel",
        "Размер" => "Size",
        "Размытие фона (акрил Windows)" => "Background blur (Windows acrylic)",
        "Режим поиска: точное имя или вхождение" => {
            "Search mode: exact name or substring"
        }
        "Результаты поиска" => "Search results",
        "Русский" => "Russian",
        "Сброс" => "Reset",
        "Сбросить индекс поиска" => "Reset search index",
        "Сбросить настройки" => "Reset settings",
        "Свернуть боковую панель" => "Collapse sidebar",
        "Свернуть панель" => "Collapse panel",
        "Свернуть" => "Minimize",
        "Свойства Windows…" => "Windows properties…",
        "Свойства" => "Properties",
        "Системный" => "System",
        "системный" => "system",
        "съёмный" => "removable",
        "локальный" => "local",
        "сетевой" => "network",
        "диск" => "drive",
        "RAM-диск" => "RAM disk",
        "Создать ярлык" => "Create shortcut",
        "Создать" => "Create",
        "Сортировка" => "Sorting",
        "Сохранить настройки" => "Save settings",
        "Список" => "List",
        "Стекло" => "Glass",
        "Таблица Excel" => "Excel spreadsheet",
        "Текстовый документ" => "Text document",
        "Текст" => "Text",
        "Текущая папка" => "Current folder",
        "Тип" => "Type",
        "Точное имя (Enter)…" => "Exact name (Enter)…",
        "Точное имя" => "Exact name",
        "Точное" => "Exact",
        "Удалить безвозвратно Shift+Del" => "Delete permanently Shift+Del",
        "Удалить безвозвратно" => "Delete permanently",
        "Удалить безвозвратно?" => "Delete permanently?",
        "Удалить в корзину        Del" => "Move to trash            Del",
        "Удалить в корзину" => "Move to trash",
        "Удалить" => "Delete",
        "Файл" => "File",
        "Фильтр внутри архива…" => "Filter inside archive…",
        "Частичное" => "Partial",
        "Читаю папку…" => "Reading folder…",
        "Чтение папки прервано" => "Folder reading interrupted",
        "Это не папка" => "This is not a folder",
        "Язык определяется по Windows." => "Language is detected from Windows.",
        "Язык" => "Language",
        "Имя (Enter — поиск в папке)…" => "Name (Enter — search folder)…",
        "Акрил Windows недоступен — стекло работает как полупрозрачный слой." => {
            "Windows acrylic is unavailable — glass works as a semi-transparent layer."
        }
        "Подтверждать безвозвратное удаление" => "Confirm permanent deletion",
        "нельзя переместить папку внутрь самой себя" => "cannot move a folder into itself",
        "источник и цель совпадают" => "source and target are the same",
        "пропущено" => "skipped",
        "Не выбраны процессы для закрытия" => "No processes selected to close",
        "системный процесс" => "system process",
        "служба" => "service",
        "защищённый процесс" => "protected process",
        "нужны права администратора" => "administrator rights required",
        "Закрытие занятых файлов поддерживается только в Windows" => {
            "Closing locked files is only supported on Windows"
        }
        "Такой папки нет или доступ запрещён" => "That folder does not exist or access is denied",
        "Выбрано:" => "Selected:",
        "Блокируют удаление:" => "Blocking deletion:",
        "Проводник будет перезапущен" => "Explorer will be restarted",
        "Удаление" => "Deleting",
        "Удаление в корзину" => "Move to trash",
        "Файл занят" => "File is locked",
        "Удалить в корзину (Del). Shift+Del — безвозвратно" => {
            "Move to trash (Del). Shift+Del — permanently"
        }
        "Удалить кэш индекса диска — следующий поиск построит его заново" => {
            "Delete the drive index cache — the next search will rebuild it"
        }
        "PowerShell не смог создать ярлык" => "PowerShell could not create the shortcut",
        "Ярлыки поддерживаются только в Windows" => "Shortcuts are only supported on Windows",
        "Корзина" => "Trash",
        "Корзина пуста" => "Trash is empty",
        "Удалённые объекты появятся здесь" => "Deleted items will appear here",
        "Исходный путь неизвестен" => "Original path unknown",
        "Очистить корзину" => "Empty trash",
        "Потоки поиска" => "Search threads",
        "Меньше потоков — ниже нагрузка на CPU, но индекс строится дольше." => {
            "Fewer threads means lower CPU usage, but indexing takes longer."
        }
        _ => text,
    }
}

#[cfg(windows)]
pub(crate) fn system_is_english() -> bool {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;

    // LANGID: младшие 10 бит — primary language. 0x19 — русский.
    let language = unsafe { GetUserDefaultUILanguage() } & 0x03ff;
    language != 0x19
}

#[cfg(not(windows))]
pub(crate) fn system_is_english() -> bool {
    std::env::var("LANG")
        .map(|value| !value.to_lowercase().starts_with("ru"))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_known_strings_and_falls_back() {
        set_english(true);
        assert_eq!(tr("Настройки"), "Settings");
        assert_eq!(tr("Удалить в корзину"), "Move to trash");
        assert_eq!(tr("Неизвестная строка"), "Неизвестная строка");
        set_english(false);
        assert_eq!(tr("Настройки"), "Настройки");
    }
}
