ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "posts", force: :cascade do |t|
    t.string "title", default: "", null: false
    t.bigint "author_id"
    t.timestamps
    t.index ["author_id"], name: "index_posts_on_author_id"
  end
end
