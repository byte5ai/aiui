import { addMessages, init, getLocaleFromNavigator, locale } from "svelte-i18n";
import de from "./de.json";
import en from "./en.json";

addMessages("de", de);
addMessages("en", en);

init({
  fallbackLocale: "en",
  initialLocale: getLocaleFromNavigator() ?? "en",
});

// #197: the locale is auto-detected, but both HTML entry points declared
// `lang="de"` unconditionally and nothing ever corrected it. An English
// session therefore got German screen-reader pronunciation and German
// spellcheck inside every `form` textarea. The HTML files now default to a
// neutral `lang="en"`; this keeps the document in step with whatever
// svelte-i18n actually resolved (including the fallback).
locale.subscribe((value) => {
  if (value && typeof document !== "undefined") {
    document.documentElement.lang = value;
  }
});
