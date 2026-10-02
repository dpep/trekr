class Widget < ActiveRecord::Base
  scope :visible, -> { where(hidden: false) }
  has_many :parts
  has_many :spares, class_name: "Part"

  def self.popular
    order(:score)
  end
end
