class CreateKennels < ActiveRecord::Migration[7.1]
  def change
    create_table :kennels do |t|
      t.text :metadata
    end
  end
end
