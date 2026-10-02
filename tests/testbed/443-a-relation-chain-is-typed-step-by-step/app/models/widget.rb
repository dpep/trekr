class Widget < ActiveRecord::Base
  scope :active, -> { where(active: true) }
  has_many :parts
end
