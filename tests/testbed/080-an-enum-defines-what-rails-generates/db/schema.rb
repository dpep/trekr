ActiveRecord::Schema[8.0].define(version: 1) do
  create_table "accounts" do |t|
    t.integer "role"
  end
end
