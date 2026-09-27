ActiveRecord::Schema.define do
  create_table :posts do |t|
    t.references :author
    t.string :title
  end
end
