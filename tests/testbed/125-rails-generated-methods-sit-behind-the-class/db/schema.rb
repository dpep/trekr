ActiveRecord::Schema.define(version: 1) do
  create_table "widgets" do |t|
    t.integer "status"
  end
  create_table "gadgets" do |t|
    t.integer "kind"
  end
end
