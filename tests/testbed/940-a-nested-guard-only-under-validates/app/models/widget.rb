class Widget
  include ActiveModel::Validations
  validates! :name, exclusion: { in: %w[x], unless: :bang_skip? }
  validates_with NameValidator, rules: { if: :rule_data }
  validates_length_of :name, maximum: 3, tokenizer: { if: :tokenizer_data }

  def bang_skip?
    false
  end

  def rule_data
  end

  def tokenizer_data
  end
end
