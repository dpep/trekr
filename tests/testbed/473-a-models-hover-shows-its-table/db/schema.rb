ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "posts", force: :cascade do |t|
    t.string "title", default: "", null: false
    t.text "body"
    t.string "type"
    t.index ["title"], name: "index_posts_on_title", unique: true
  end

  create_table "legacy_widgets", force: :cascade do |t|
    t.string "label"
  end

  create_table "admin_reports", force: :cascade do |t|
    t.string "summary"
  end
end
