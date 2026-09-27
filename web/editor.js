// The editor (placeholder until the collaborative editor is ported): the
// text is an ordinary form; Ctrl+S / Cmd+S saves the draft.
const form = document.getElementById("post-form");
if (form) {
  document.addEventListener("keydown", (ev) => {
    if ((ev.ctrlKey || ev.metaKey) && ev.key === "s") {
      ev.preventDefault();
      form.requestSubmit(document.querySelector("button[form=post-form][value=save]") || undefined);
    }
  });
}
