class Order < ActiveRecord::Base
  has_many :line_items
  included { has_one :"#{name.underscore}_search_data" }
end
