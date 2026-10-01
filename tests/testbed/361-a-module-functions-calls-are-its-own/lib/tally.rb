module Tally
  module_function

  def count_words(text)
    text.split.size
  end

  def lonely
    :lonely
  end
end

class Report
  def size_of(text)
    Tally.count_words(text)
  end
end
