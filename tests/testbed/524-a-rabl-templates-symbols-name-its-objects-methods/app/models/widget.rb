class Widget < ActiveRecord::Base
  has_many :parts

  def label
    "w"
  end
end
