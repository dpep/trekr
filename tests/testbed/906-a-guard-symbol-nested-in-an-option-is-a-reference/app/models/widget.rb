class Widget
  include ActiveModel::Validations
  validates :name, exclusion: { in: %w[x], unless: :skip_name? }
  validates :size, numericality: { greater_than: :min_size, if: :sized? }

  def skip_name?
    false
  end

  def min_size
    1
  end

  def sized?
    true
  end

  def lonely
  end
end
