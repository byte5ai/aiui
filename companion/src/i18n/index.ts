import { addMessages, init, getLocaleFromNavigator } from "svelte-i18n";
import de from "./de.json";
import en from "./en.json";

addMessages("de", de);
addMessages("en", en);

init({
  fallbackLocale: "en",
  initialLocale: getLocaleFromNavigator() ?? "en",
});
