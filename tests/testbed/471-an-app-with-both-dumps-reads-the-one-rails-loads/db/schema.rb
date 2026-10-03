ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "widgets", force: :cascade do |t|
    t.text "code"
  end
end
