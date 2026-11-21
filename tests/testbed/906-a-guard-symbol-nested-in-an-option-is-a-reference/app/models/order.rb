class Order
  include AASM

  aasm do
    state :pending, initial: true
    state :paid

    event :pay, after: :notify_paid do
      transitions from: :pending, to: :paid, guard: :payable?, after: :record_payment
    end
  end

  def notify_paid
  end

  def payable?
    true
  end

  def record_payment
  end

  def lonely
  end
end
